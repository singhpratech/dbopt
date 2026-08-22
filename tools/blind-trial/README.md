# Blind production trial

A reproducible, *graded* trial of the connected advisor: a fresh database with
18 planted performance defects and 5 benign look-alikes, a 4-minute mixed
workload so the DMVs and Query Store fill, and a ground-truth table with the
measured evidence for each defect (logical reads before/after the ideal fix,
fragmentation, modification counters…).

The point is that the grader and the product are independent: the database
was built by someone who had not read dbopt's rules, and dbopt was run against
it by someone who had not read `ground_truth.md`. Run it yourself and you can
grade the tool without trusting either of us.

```bash
export DBOPT_SERVER='localhost,1433' DBOPT_USER=sa DBOPT_PASSWORD='…'
source env.sh
sqm -i 01_schema_data.sql      # ~15 s: creates BlindTrial (DROPS it if present)
sq  -i 02_indexes_procs.sql
./run_workload.sh 240          # 4 parallel loops; populates DMVs + Query Store
sq  -i 05_confirm.sql          # proves missing-index / usage DMVs have rows
```

Then point dbopt at `BlindTrial` (Health, Advise, and the DB-wide scan) and
compare against `ground_truth.md`. `03_measure.sql` / `04_physical.sql`
re-measure the evidence on your instance.

Requires a SQL Server 2019+ you are allowed to create a database on (we use the
2025 Docker image). Counters reset when the instance restarts — rerun the
workload before grading if `counter_age_secs` is small.

Result on 2026-08-22 (dbopt 0.4.3 → 0.4.4): first run 11 hits / 2 partial / 6
misses / 5 false alarms; after the fix wave, 18 / 18 with no false alarm on the
five look-alikes. The misses were real product gaps — fragmentation, fill
factor, forwarded records, stale statistics, parameter-sniffing skew, deprecated
LOB columns, and a query-level `PlanAffectingConvert` the plan parser dropped.
