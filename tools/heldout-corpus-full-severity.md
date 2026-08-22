# Full-severity held-out corpus (2026-08-22)

Sources for the per-rule, all-severity false-positive triage quoted in the README. `heldout-corpus-full-severity-triage.md` is the per-rule table after the fix wave (TP = valid advice, FP = wrong in context, NOISE = true but useless here).

# Corpus sources (fetched 2026-08-22)

All files fetched via raw.githubusercontent.com / git clone at the commits below. None of these files are in samples/scenarios.

| dir | repo | ref / commit | files |
|---|---|---|---|
| frk/ | https://github.com/BrentOzarULTD/SQL-Server-First-Responder-Kit | main @ 756206859c23aa98cdb41643763c5f1d3c10cbab | sp_Blitz, sp_BlitzCache, sp_BlitzIndex, sp_BlitzFirst, sp_BlitzLock, sp_BlitzWho, sp_BlitzBackups, sp_BlitzAnalysis, sp_DatabaseRestore, sp_ineachdb |
| ola/ | https://github.com/olahallengren/sql-server-maintenance-solution | master @ be63a5e57887b4b2bd388621dfd7acb5ef6a6564 | CommandExecute, DatabaseBackup, DatabaseIntegrityCheck, IndexOptimize, CommandLog, MaintenanceSolution |
| whoisactive/ | https://github.com/amachanic/sp_whoisactive | master @ 40ac17b8e61a9ae8ead6410ec51c74a480636ea0 | sp_WhoIsActive.sql |
| tsqlt/ | https://github.com/tSQLt-org/tSQLt | main @ 4a921d0dacfb1d66b3db124c58158c80e5e910e6 | Source/*.sql larger than 2 KB (23 files) |
| mss/ | https://github.com/microsoft/sql-server-samples | 1ab31bc560415b570d57bb5ff9896f4698891321 | adventure-works/oltp-install-script/instawdb.sql, northwind-pubs/instnwnd.sql + instpubs.sql, features/json/*.sql |
| darling/ | https://github.com/erikdarlingdata/DarlingData | main @ 6165b66a251504d5b88edd10416166d9d1d114d3 | sp_HumanEvents, sp_PressureDetector, sp_QuickieStore, sp_HealthParser, sp_LogHunter, sp_IndexCleanup |

Total: 53 files, 6.6M.
