# CedroDB vs SQLite — reproducible comparison

This crate measures CedroDB and SQLite on the same deterministic JSON-document workloads. It lives outside the published `cedrodb` crate so the engine keeps its small dependency set; SQLite is built from the bundled source pinned in `Cargo.lock`.

**It is evidence, not marketing.** Read [How to read the numbers](#how-to-read-the-numbers) before quoting any figure.

## Run it

```sh
cd benchmarks/sqlite-comparison
./run.sh my-machine                                   # 3 runs each for 10,000 and 100,000 documents
python3 summarize.py results/*-my-machine-100000-run*.json   # median [min–max] across runs
```

Publish medians of several runs, never a single run: one run can be disturbed by the machine (an early single run of this harness showed a 2× outlier that disappeared when repeated). `summarize.py` refuses any run whose verification failed. For one-off runs use `cargo run --release -- --records 10000 > out.json`.

| Option | Default | Meaning |
| --- | ---: | --- |
| `--records` | 10000 | Documents in the main dataset |
| `--iterations` | 20 | Repetitions of the aggregate query |
| `--seed` | 42 | Seed of the data generator (same data every run) |
| `--single-commit-limit` | 1000 | Cap for the "one document per commit" insert, which waits for the disk on every commit |
| `--dir` | system temp | Where databases are created. **Point it at the disk you want to measure** |
| `--engine` | all | Internal: run one engine in the current process |
| `--reopen <dir>` | – | Open an existing CedroDB directory once and print the reopen time and peak RSS. Alternate it between two builds of the engine, in fresh processes and on the same log, to attribute a change to a code difference |

Always use `--release`: a debug build is reported as `build: debug (NOT valid for comparisons)`.

## What is measured

Every engine runs in its **own process** so peak RSS is not contaminated by another engine. Each run:

1. Inserts the dataset with 1, 100 and 1,000 documents per commit (batch 1 is capped by `--single-commit-limit`).
2. Verifies the stored data (aggregate against an in-memory model, sampled documents byte-for-byte). A run whose verification fails is reported with `verified: false` and must not be used.
3. Reads 5,000 existing keys and 5,000 absent keys (warm cache), recording each latency.
4. Aggregates `count/sum/min/max` of one numeric field.
5. Updates 10% of the documents and deletes another 10% in commits of 100 documents.
6. Closes and reopens the database, then verifies the state again.

## Engines and durability

| Engine row | Configuration | Durable per commit? |
| --- | --- | --- |
| `cedrodb` | append-only log, `File::sync_all` on every commit | yes (cannot be disabled) |
| `sqlite-wal-full` | `journal_mode=WAL`, `synchronous=FULL`, `fullfsync=ON` | yes |
| `sqlite-delete-full` | `journal_mode=DELETE`, `synchronous=FULL`, `fullfsync=ON` | yes |
| `sqlite-wal-normal` | `journal_mode=WAL`, `synchronous=NORMAL`, `fullfsync=ON` | **no** — shown only to illustrate how much a relaxed contract changes SQLite |

`PRAGMA fullfsync=ON` matters on macOS: Rust's `sync_all` flushes the device cache (`F_FULLFSYNC`), while SQLite's default is a plain `fsync`. Without it SQLite would appear faster only because it promises less.

SQLite stores each document as JSON text in `docs(key TEXT PRIMARY KEY, body TEXT)` and aggregates with `json_extract`. **A SQLite schema with generated columns or an index on the numeric field would make the aggregate far faster**, so the aggregate row compares two document-store designs, not the best possible SQLite. Reads return `serde_json::Value` for both engines, so JSON decoding is part of every read for both.

## How to read the numbers

- **Compare CedroDB only with the `*-full` rows.** `sqlite-wal-normal` is not equivalent.
- **Durable writes are dominated by the disk**, not by the engine: both engines wait for a flush per commit, so single-document commit rates are close to each other and to the device's flush latency.
- **Reads and aggregates are in-memory for CedroDB** (all live documents stay in RAM). That is a design choice with a cost: peak RSS and the 64 MiB default live-state limit. SQLite reads from its page cache and can handle data larger than RAM.
- **Reopen time is linear in log size** for CedroDB (it replays the whole log); SQLite opens the file.
- **Caches are not controlled** (no cold-start mode) and the OS page cache is warm.
- **Single machine, single disk, synthetic documents.** Repeat on your own hardware and report the whole JSON file with the environment block.
- **No claim of general superiority follows from these results.** They show where each engine's design helps or hurts on this workload.

## Results

Documents are generated per batch, through the same path for both engines, and are not retained or cloned by the harness; the verification model holds only a revision per key. Peak RSS therefore reflects the engine plus a small constant harness overhead. Latest run (`results/2026-10-18-apple-m2-batch2-*`, Apple M2, 100,000 documents, median of 3): peak RSS 133.1 MiB [132.1–133.4] for CedroDB vs 13.5 MiB for SQLite WAL+FULL (10,000 documents: 27.6 vs 11.6 MiB). **Correction:** the first published run (`results/superseded/`) reported 257 MiB vs ~100 MiB because the harness held the whole generated dataset and cloned each value; those RSS figures were harness artifacts and are superseded (see `results/superseded/README.md`).

Result files live in [`results/`](results) as `<date>-<machine>-<records>.json` (full report) plus the `.md` summary produced by the same run.
