# Superseded results

The `2026-10-18-apple-m2-*` files were the first published run. Their **peak RSS values are not valid for the engines**:
that harness built the whole dataset in memory (plus a `value.clone()` per CedroDB put) before committing, so the
reported peak RSS (CedroDB 257 MiB, SQLite about 100 MiB at 100,000 documents) included harness data. Timings were not
affected by the change (generation was outside the timed region). They are kept as published evidence; use
`2026-10-18-apple-m2-batch2-*` instead.
