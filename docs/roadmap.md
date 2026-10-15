# Road to 1.0

Each release must solve a concrete application problem, describe failures and pass Rust/Node/Python interoperability checks. Future milestones are plans, not implemented features or promised dates.

| Release | Application problem | Evidence gate |
| --- | --- | --- |
| 0.2 | Keep local documents and events together | Atomic frames, cursors, index/CAS/SDK tests |
| 0.3 | Recover offline data before upgrades | Checked complete backups, no-overwrite restoration, corruption/truncation tests |
| 0.4 | Reserve stock only when every dependency is unchanged | Conditional multi-document batches, conflict and concurrent-contender tests |
| 0.5 | Resume interrupted local background jobs | Durable leases, scheduling, bounded retries, stale-worker fencing, dead letters |
| 0.6 planned | Import external datasets predictably | Bounded ingestion, reject records, validation and restart checkpoints |
| 0.7 planned | Establish storage failure behavior | I/O fault injection, process interruption, fuzzing and reproductions |
| 0.8 planned | Make measured performance decisions | Published latency/RSS/recovery workloads and methodology |
| 0.9 planned | Exercise compatibility contracts | Format/API review, upgrade matrix, restore drills and migration notes |
| 1.0 candidate | Support a stable contract | All gates below, with evidence and a maintenance policy |

## Before stable 1.0

1. Specify and test durability, isolation, concurrency and resource boundaries on supported platforms.
2. Fuzz log/backup decoders and SDK inputs: malformed sizes, Unicode, depth and numeric interoperability.
3. Reproduce append/sync/backup/restore failures. Distinguish process interruption from actual power-loss certification.
4. Publish a format specification, migration strategy and compatibility tests using historical fixtures.
5. Define log growth and cursor retention before adding compaction; never silently invalidate consumer checkpoints.
6. Publish reproducible benchmarks including hardware, versions, memory and unfavorable results.
7. Audit native distributions, platform support, installation without compilers and standalone source rebuilds.
8. Establish typed APIs, error contracts, uncertain-commit handling, security reporting and maintenance expectations.

SQL, vector search and distributed replication are not required for this product direction. New features must not replace testing the guarantees already offered.
