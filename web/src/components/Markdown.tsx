import { useState } from "react";

/**
 * Tiny, dependency-free Markdown renderer for AI chat output.
 *
 * Why hand-rolled: the rest of the app is deliberately lean on deps, and we only
 * need the subset LLMs actually emit — fenced code blocks, inline code, bold,
 * italic, headings, lists, blockquotes, links, rules. It builds real React
 * elements (never `dangerouslySetInnerHTML`), so it is XSS-safe by construction:
 * any HTML the model emits is shown as literal text, and only http(s) links
 * become anchors.
 *
 * It tolerates partial input (used live while a response streams) — an unclosed
 * code fence just renders as a growing code block until the closing fence
 * arrives and the parse settles.
 */

type Align = "left" | "center" | "right" | null;
type Block =
  | { type: "code"; lang: string; content: string }
  | { type: "heading"; level: number; text: string }
  | { type: "hr" }
  | { type: "ul"; items: string[] }
  | { type: "ol"; items: string[] }
  | { type: "quote"; text: string }
  | { type: "table"; header: string[]; align: Align[]; rows: string[][] }
  | { type: "p"; text: string };

const RE_FENCE = /^```(.*)$/;
const RE_HEAD = /^(#{1,6})\s+(.*)$/;
const RE_HR = /^\s*(?:-{3,}|\*{3,}|_{3,})\s*$/;
const RE_UL = /^\s*[-*+]\s+/;
const RE_OL = /^\s*\d+[.)]\s+/;
const RE_QUOTE = /^\s*>\s?/;
// A GFM table separator row, e.g. `| --- | :--: | --: |` (dashes/colons/pipes only).
const RE_TSEP = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;

/** Split one `| a | b |` table row into trimmed cells (tolerates missing edge pipes). */
function splitRow(line: string): string[] {
  let s = line.trim();
  if (s.startsWith("|")) s = s.slice(1);
  if (s.endsWith("|")) s = s.slice(0, -1);
  return s.split("|").map((c) => c.trim());
}

/** Column alignment from a separator cell (`:--` left, `:-:` center, `--:` right). */
function cellAlign(sep: string): Align {
  const s = sep.trim();
  const l = s.startsWith(":");
  const r = s.endsWith(":");
  return l && r ? "center" : r ? "right" : l ? "left" : null;
}

/** A table starts where a row containing `|` is immediately followed by a separator row. */
function isTableStart(lines: string[], i: number): boolean {
  return i + 1 < lines.length && lines[i].includes("|") && RE_TSEP.test(lines[i + 1]);
}

function isBlockStart(line: string): boolean {
  return (
    RE_FENCE.test(line.trim()) ||
    RE_HEAD.test(line) ||
    RE_HR.test(line) ||
    RE_UL.test(line) ||
    RE_OL.test(line) ||
    RE_QUOTE.test(line)
  );
}

function parseBlocks(src: string): Block[] {
  const lines = src.replace(/\r\n/g, "\n").split("\n");
  const blocks: Block[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];

    // Fenced code block — capture verbatim until the closing fence (or EOF).
    const fence = RE_FENCE.exec(line.trim());
    if (fence) {
      const lang = fence[1].trim();
      const buf: string[] = [];
      i++;
      while (i < lines.length && !/^```/.test(lines[i].trim())) {
        buf.push(lines[i]);
        i++;
      }
      i++; // consume the closing fence (no-op at EOF)
      blocks.push({ type: "code", lang, content: buf.join("\n") });
      continue;
    }

    if (line.trim() === "") {
      i++;
      continue;
    }

    const h = RE_HEAD.exec(line);
    if (h) {
      blocks.push({ type: "heading", level: h[1].length, text: h[2].trim() });
      i++;
      continue;
    }

    if (RE_HR.test(line)) {
      blocks.push({ type: "hr" });
      i++;
      continue;
    }

    if (RE_UL.test(line)) {
      const items: string[] = [];
      while (i < lines.length && RE_UL.test(lines[i])) {
        items.push(lines[i].replace(RE_UL, ""));
        i++;
      }
      blocks.push({ type: "ul", items });
      continue;
    }

    if (RE_OL.test(line)) {
      const items: string[] = [];
      while (i < lines.length && RE_OL.test(lines[i])) {
        items.push(lines[i].replace(RE_OL, ""));
        i++;
      }
      blocks.push({ type: "ol", items });
      continue;
    }

    if (RE_QUOTE.test(line)) {
      const buf: string[] = [];
      while (i < lines.length && RE_QUOTE.test(lines[i])) {
        buf.push(lines[i].replace(RE_QUOTE, ""));
        i++;
      }
      blocks.push({ type: "quote", text: buf.join("\n") });
      continue;
    }

    // GFM table: header row + `|---|` separator, then rows until a blank/other block.
    if (isTableStart(lines, i)) {
      const header = splitRow(lines[i]);
      const align = splitRow(lines[i + 1]).map(cellAlign);
      i += 2;
      const rows: string[][] = [];
      while (
        i < lines.length &&
        lines[i].trim() !== "" &&
        lines[i].includes("|") &&
        !isBlockStart(lines[i])
      ) {
        rows.push(splitRow(lines[i]));
        i++;
      }
      blocks.push({ type: "table", header, align, rows });
      continue;
    }

    // Paragraph: gather until a blank line or the start of another block.
    const buf: string[] = [];
    while (
      i < lines.length &&
      lines[i].trim() !== "" &&
      !isBlockStart(lines[i]) &&
      !isTableStart(lines, i)
    ) {
      buf.push(lines[i]);
      i++;
    }
    blocks.push({ type: "p", text: buf.join("\n") });
  }

  return blocks;
}

// Inline spans, matched leftmost-first; each alternative is one capture group:
//   1 `code`   2 ***bold-italic***   3 **bold**/__bold__   4 *italic*   5 _italic_   6 [text](url)
// Code is first so markup inside backticks stays literal. Emphasis spans require
// a non-space just inside the markers (so "a * b" isn't italics).
//
// The `_italic_` form (group 5) is gated by word boundaries — `(?<![\w])_…_(?![\w])`
// — because intra-word underscores are NOT emphasis in CommonMark/GFM. Without
// this, SQL identifiers like `counter_name` and `sys.dm_os_performance_counters`
// get mangled into "counter*name*" / "dmosperformancecounters". Asterisk emphasis
// is left intra-word-capable, matching the spec.
const RE_INLINE =
  /(`[^`]+`)|(\*\*\*(?!\s)[\s\S]+?(?<!\s)\*\*\*)|(\*\*(?!\s)[\s\S]+?(?<!\s)\*\*|__(?!\s)[\s\S]+?(?<!\s)__)|(\*(?!\s)[^*\n]+?(?<!\s)\*)|((?<![A-Za-z0-9_])_(?!\s)[^_\n]+?(?<!\s)_(?![A-Za-z0-9_]))|(\[[^\]]+\]\([^)\s]+\))/;

function renderInline(text: string): React.ReactNode[] {
  const out: React.ReactNode[] = [];
  let rest = text;
  let key = 0;
  // Walk the string, peeling off the leftmost inline token each pass.
  // (Global regex with lastIndex is avoided so recursion is simple/stateless.)
  // eslint-disable-next-line no-constant-condition
  while (true) {
    const m = RE_INLINE.exec(rest);
    if (!m) {
      if (rest) out.push(rest);
      break;
    }
    if (m.index > 0) out.push(rest.slice(0, m.index));
    const tok = m[0];
    if (m[1]) {
      out.push(<code key={key++}>{tok.slice(1, -1)}</code>);
    } else if (m[2]) {
      // ***bold-italic*** → strong wrapping em
      out.push(
        <strong key={key++}>
          <em>{renderInline(tok.slice(3, -3))}</em>
        </strong>,
      );
    } else if (m[3]) {
      out.push(<strong key={key++}>{renderInline(tok.slice(2, -2))}</strong>);
    } else if (m[4] || m[5]) {
      out.push(<em key={key++}>{renderInline(tok.slice(1, -1))}</em>);
    } else if (m[6]) {
      const link = /^\[([^\]]+)\]\(([^)\s]+)\)$/.exec(tok);
      if (link && /^https?:\/\//i.test(link[2])) {
        out.push(
          <a key={key++} href={link[2]} target="_blank" rel="noopener noreferrer">
            {link[1]}
          </a>,
        );
      } else {
        out.push(tok); // not a safe http(s) link — leave literal
      }
    }
    rest = rest.slice(m.index + tok.length);
  }
  return out;
}

/** Inline render that turns single newlines into soft line breaks. */
function renderMultiline(text: string): React.ReactNode[] {
  const parts = text.split("\n");
  const out: React.ReactNode[] = [];
  parts.forEach((p, i) => {
    out.push(...renderInline(p));
    if (i < parts.length - 1) out.push(<br key={`br${i}`} />);
  });
  return out;
}

function CodeBlock({ lang, content }: { lang: string; content: string }) {
  const [copied, setCopied] = useState(false);
  async function copy() {
    try {
      await navigator.clipboard?.writeText(content);
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    } catch {
      /* clipboard may be blocked in non-secure contexts */
    }
  }
  return (
    <div className="md-code">
      <div className="md-code-head">
        <span className="md-code-lang">{lang || "code"}</span>
        <button className={`md-code-copy${copied ? " copied" : ""}`} onClick={copy} title="Copy this code block">
          {copied ? "COPIED ✓" : "COPY"}
        </button>
      </div>
      <pre>
        <code>{content}</code>
      </pre>
    </div>
  );
}

function renderBlock(b: Block, key: number): React.ReactNode {
  switch (b.type) {
    case "code":
      return <CodeBlock key={key} lang={b.lang} content={b.content} />;
    case "heading":
      return (
        <div key={key} className={`md-h md-h${Math.min(b.level, 6)}`}>
          {renderInline(b.text)}
        </div>
      );
    case "hr":
      return <hr key={key} className="md-hr" />;
    case "ul":
      return (
        <ul key={key} className="md-ul">
          {b.items.map((it, j) => (
            <li key={j}>{renderInline(it)}</li>
          ))}
        </ul>
      );
    case "ol":
      return (
        <ol key={key} className="md-ol">
          {b.items.map((it, j) => (
            <li key={j}>{renderInline(it)}</li>
          ))}
        </ol>
      );
    case "quote":
      return (
        <blockquote key={key} className="md-quote">
          {renderMultiline(b.text)}
        </blockquote>
      );
    case "table":
      return (
        <div key={key} className="md-table-wrap">
          <table className="md-table">
            <thead>
              <tr>
                {b.header.map((h, j) => (
                  <th key={j} style={b.align[j] ? { textAlign: b.align[j]! } : undefined}>
                    {renderInline(h)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {b.rows.map((row, r) => (
                <tr key={r}>
                  {b.header.map((_, c) => (
                    <td key={c} style={b.align[c] ? { textAlign: b.align[c]! } : undefined}>
                      {renderInline(row[c] ?? "")}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    case "p":
      return (
        <p key={key} className="md-p">
          {renderMultiline(b.text)}
        </p>
      );
  }
}

export function Markdown({ text }: { text: string }) {
  const blocks = parseBlocks(text);
  return <div className="md">{blocks.map((b, i) => renderBlock(b, i))}</div>;
}
