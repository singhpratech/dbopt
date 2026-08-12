#!/usr/bin/env python3
"""Measure FALSE NEGATIVES by planting known defects in real third-party T-SQL.

Both existing quality signals only measure one direction:

  * the eval corpus is hand-authored, so it proves "no regression on the cases
    we wrote";
  * the held-out corpus (tools/heldout-corpus.sh) is real code we did not write,
    so a high-severity finding on it is a false-positive candidate.

Neither can count what the analyzer *failed* to say. You cannot measure a miss
against code whose defects you do not already know.

So: take the held-out files, inject a defect whose rule id is known by
construction, and check the analyzer reports that rule ON THAT LINE. Recall =
caught / injected. Because the host file is real code with its own comments,
strings, batches and nesting, this measures the analyzer in context — which is
exactly where every false negative found so far has lived (a missing semicolon,
a comment inside a clause, a multi-line string shifting the line counter, a CTE
in an earlier batch).

Deterministic: injection sites are chosen by scanning, never randomly, so the
number is reproducible and a regression is attributable.

Usage:  tools/seeded-defects.py [corpus_dir] [--verbose]
        (corpus_dir defaults to target/heldout; run tools/heldout-corpus.sh first)
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile

BIN = os.environ.get("DBOPT_BIN", "./target/release/dbopt")

# (rule id, SQL to inject). Each snippet is a single line so the expected line
# number is unambiguous, and each is unmistakably the defect its rule describes.
DEFECTS: list[tuple[str, str]] = [
    ("hygiene.unbounded_dml",
     "UPDATE dbo.SeededDefectTable SET SeededFlag = 1;"),
    ("hygiene.nolock",
     "SELECT SeededId FROM dbo.SeededDefectTable WITH (NOLOCK);"),
    ("hygiene.select_star",
     "SELECT * FROM dbo.SeededDefectTable;"),
    ("sarg.function_on_column",
     "SELECT SeededId FROM dbo.SeededDefectTable WHERE UPPER(SeededName) = 'X';"),
    ("sarg.leading_wildcard",
     "SELECT SeededId FROM dbo.SeededDefectTable WHERE SeededName LIKE '%abc';"),
    ("sarg.not_in_nullable",
     "SELECT SeededId FROM dbo.SeededDefectTable WHERE SeededId NOT IN (SELECT SeededRef FROM dbo.SeededOther);"),
    ("joins.join_without_on",
     "SELECT a.SeededId FROM dbo.SeededA a JOIN dbo.SeededB b;"),
    ("joins.comma_cross_join",
     "SELECT a.SeededId FROM dbo.SeededA a, dbo.SeededB b WHERE a.SeededId = b.SeededId;"),
    ("security.xp_cmdshell",
     "EXEC master..xp_cmdshell 'dir C:\\';"),
    ("security.grant_to_public",
     "GRANT SELECT ON dbo.SeededDefectTable TO PUBLIC;"),
    ("deprecated.sp_dboption",
     "EXEC sp_dboption 'SeededDb', 'autoclose', 'true';"),
    ("locking.set_transaction_isolation_read_uncommitted",
     "SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED;"),
    ("antipattern.union_should_be_union_all",
     "SELECT SeededId FROM dbo.SeededA UNION SELECT SeededId FROM dbo.SeededB;"),
    ("modern.missing_schema_prefix",
     "SELECT SeededId FROM SeededUnqualifiedTable WHERE SeededId = 1;"),
    ("datatype.float_for_money",
     "CREATE TABLE dbo.SeededMoney (SeededId int NOT NULL, TotalAmount float NOT NULL);"),
]


# Hostile *preceding* context, prepended immediately before the injected defect.
#
# Injecting into clean surroundings measures very little: every false negative
# found in this analyzer so far has come from what sat immediately BEFORE the
# statement — a preceding `ON`, a missing semicolon, a comment between clause
# keywords, an identifier that spells a keyword. Without these, a harness can
# report perfect recall while a known blocker is live, which is exactly what
# happened the first time this script was validated.
CONTEXTS: list[tuple[str, list[str]]] = [
    ("plain", []),
    # `SET NOCOUNT ON` before DML once silenced the critical rule outright,
    # because the preceding token was `ON`.
    ("after-set-on", ["SET NOCOUNT ON"]),
    ("after-quoted-ident-option", ["SET QUOTED_IDENTIFIER ON"]),
    # An unterminated statement: forward and backward scans must stop at the
    # statement boundary rather than borrowing this one's clauses.
    ("after-unterminated-select", ["SELECT 1 AS Probe"]),
    ("after-unterminated-update", ["UPDATE dbo.OtherTable SET Flag = 0 WHERE Id = 1"]),
    # A comment directly before the statement.
    ("after-inline-comment", ["/* preceding note */"]),
    # An identifier that spells a keyword, in a prior unterminated statement.
    ("after-keyword-ident", ["SELECT RunId, [Merge], [Select] FROM dbo.RunFlags"]),
    # A CTE belonging to a previous statement must not vouch for this one.
    ("after-foreign-cte", ["WITH q AS (SELECT Id FROM dbo.T WHERE Id > 0) SELECT Id FROM q;"]),
    # A closed multi-line string immediately before.
    ("after-multiline-literal", ["DECLARE @doc nvarchar(max) = 'line one", "line two';"]),
]


# Used to prove an injection site is live code before anything is measured
# there. TWO canaries, because one is not enough:
#
#   * the word-level one proves we are not inside a string or comment;
#   * the statement-level one proves the site accepts a whole statement — a
#     line-based scan cannot tell that it has landed in the middle of another
#     CREATE TABLE's column list, where injecting a statement produces SQL that
#     is simply invalid, and the analyzer is right to make nothing of it.
#
# Neither canary rule appears in DEFECTS, so validating a site can never be
# circular with what is being measured there.
CANARIES: list[tuple[str, str]] = [
    ("security.xp_cmdshell", "EXEC master..xp_cmdshell 'canary';"),
    ("ddl.varchar_max_overuse",
     "CREATE TABLE dbo.SeededCanaryTbl (SeededBody nvarchar(max) NOT NULL);"),
]


def injection_sites(lines: list[str]) -> list[tuple[str, int]]:
    """Deterministic, structurally interesting places to inject.

    These are chosen to be exactly the contexts that have hidden real false
    negatives: after a batch separator, immediately after a multi-line string
    literal or block comment (where the line counter used to drift), inside a
    BEGIN/END block, and directly after a statement that omits its semicolon.
    """
    sites: list[tuple[str, int]] = []
    depth = 0  # approximate paren nesting; a statement can only go at depth 0

    def add(name: str, idx: int) -> None:
        if depth != 0:
            return
        if 0 <= idx < len(lines) and not any(s[0] == name for s in sites):
            sites.append((name, idx))

    in_str = False
    str_start = -1
    for i, line in enumerate(lines):
        stripped = line.strip()
        # Track nesting so we never offer a site inside another statement's
        # parenthesised body — injecting a whole statement into a column list
        # yields invalid SQL, and "the analyzer ignored invalid SQL" is not a
        # false negative.
        depth += line.count("(") - line.count(")")
        if depth < 0:
            depth = 0
        # after the first GO (a fresh batch)
        if stripped.upper() == "GO":
            add("after-batch-separator", i)
        # after a BEGIN that opens a block
        if stripped.upper() == "BEGIN" or stripped.upper().startswith("BEGIN "):
            add("inside-begin-block", i)
        # after the close of a multi-line block comment
        if "*/" in line and "/*" not in line:
            add("after-block-comment", i)
        # after a multi-line single-quoted string literal closes
        q = line.count("'") - line.count("''") * 2
        if q % 2 == 1:
            if not in_str:
                in_str, str_start = True, i
            else:
                in_str = False
                if i > str_start + 1:
                    add("after-multiline-string", i)
        # after a statement that omits its semicolon
        if (stripped.upper().startswith("SELECT ")
                and not stripped.endswith(";")
                and not stripped.endswith(",")
                and i + 1 < len(lines)
                and not lines[i + 1].strip()):
            add("after-unterminated-statement", i)
    depth = 0
    add("end-of-file", len(lines) - 1)
    return sites


def run_lint(path: str) -> list[dict]:
    proc = subprocess.run([BIN, "lint", path, "--format", "json"],
                          capture_output=True, text=True)
    if not proc.stdout.strip():
        return []
    try:
        return json.loads(proc.stdout)["findings"]
    except (json.JSONDecodeError, KeyError):
        return []


def main() -> int:
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    verbose = "--verbose" in sys.argv
    corpus = args[0] if args else "target/heldout"

    if not os.path.isdir(corpus):
        print(f"corpus not found: {corpus}\nrun tools/heldout-corpus.sh first", file=sys.stderr)
        return 2
    if not os.access(BIN, os.X_OK):
        print(f"build first: cargo build --workspace --release ({BIN} not executable)", file=sys.stderr)
        return 2

    files = sorted(f for f in os.listdir(corpus) if f.endswith(".sql"))
    if not files:
        print(f"no .sql files in {corpus}", file=sys.stderr)
        return 2

    injected = caught = 0
    skipped_sites = 0
    misses: list[tuple[str, str, str]] = []

    def lint_injected(tmp: str, lines: list[str], at: int, snippet: str,
                      prefix: list[str] | None = None) -> tuple[list[dict], int]:
        pre = prefix or []
        body = lines[:at + 1] + [""] + pre + [snippet, ""] + lines[at + 1:]
        expect_line = at + 3 + len(pre)  # 1-based line the injected statement lands on
        out = os.path.join(tmp, "seeded.sql")
        with open(out, "w", encoding="utf-8") as fh:
            fh.write("\n".join(body) + "\n")
        return run_lint(out), expect_line

    with tempfile.TemporaryDirectory() as tmp:
        for fname in files:
            src = open(os.path.join(corpus, fname), encoding="utf-8", errors="replace").read()
            lines = src.splitlines()
            sites = injection_sites(lines)
            for site_name, at in sites:
                # Validate the site before trusting it. Line-based scanning
                # cannot reliably tell live code from the inside of a 300-line
                # XML string literal, and code injected into a string SHOULD be
                # ignored — counting that as a miss would measure this script's
                # parser, not the analyzer's. A canary that fails to fire means
                # the site is not live code, so the whole site is skipped.
                live = True
                for c_rule, c_sql in CANARIES:
                    hits, expect_line = lint_injected(tmp, lines, at, c_sql)
                    if not any(h["rule"] == c_rule and h["line"] == expect_line for h in hits):
                        live = False
                        break
                if not live:
                    skipped_sites += 1
                    continue
                for ctx_name, prefix in CONTEXTS:
                    for rule, snippet in DEFECTS:
                        hits, expect_line = lint_injected(tmp, lines, at, snippet, prefix)
                        injected += 1
                        if any(h["rule"] == rule and h["line"] == expect_line for h in hits):
                            caught += 1
                        else:
                            # did the rule fire at all, just on the wrong line?
                            wrong_line = next((h["line"] for h in hits if h["rule"] == rule), None)
                            why = f"reported line {wrong_line}" if wrong_line else "not reported"
                            misses.append((f"{fname}:{site_name}/{ctx_name}", rule, why))

    recall = caught / injected if injected else 0.0
    print()
    print("─── seeded-defect recall ───")
    print()
    print(f"  corpus            {corpus} ({len(files)} files)")
    print(f"  sites skipped     {skipped_sites} (site rejected by canary: not live statement position)")
    print(f"  defects injected  {injected}")
    print(f"  caught on-line    {caught}")
    print(f"  recall            {recall:.3f}")
    if misses:
        print()
        print(f"  MISSES ({len(misses)}) — each is a false negative in real code:")
        shown = misses if verbose else misses[:25]
        for where, rule, why in shown:
            print(f"    {where:<46} {rule:<52} {why}")
        if len(misses) > len(shown):
            print(f"    … and {len(misses) - len(shown)} more (--verbose for all)")
    print()
    return 0 if not misses else 1


if __name__ == "__main__":
    sys.exit(main())
