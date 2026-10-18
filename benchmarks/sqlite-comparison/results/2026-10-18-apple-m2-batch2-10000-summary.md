CedroDB 0.8.0 vs SQLite 3.46.0 · release · Darwin 25.6.0 arm64 · Apple M2
records=10000 · 3 runs · median [min–max]

| metric | cedrodb | sqlite-wal-full | sqlite-delete-full | sqlite-wal-normal |
| --- | ---: | ---: | ---: | ---: |
| insert docs/s · 1 per commit | 230 [215–241] | 214 [204–230] | 69 [69–69] | 21,470 [21,093–22,002] |
| insert docs/s · 100 per commit | 18,783 [18,143–18,849] | 16,816 [14,040–17,484] | 6,792 [5,665–6,940] | 253,356 [235,508–253,387] |
| insert docs/s · 1000 per commit | 144,125 [133,968–153,185] | 134,592 [108,049–151,349] | 55,388 [47,616–55,400] | 634,916 [465,528–647,710] |
| commit p50 ms · 1 per commit | 4.06 [4.04–4.11] | 4.13 [4.05–4.76] | 13.97 [13.88–14.00] | 0.02 [0.02–0.02] |
| get hit p50 µs | 0.4 [0.4–0.7] | 2.6 [2.5–2.8] | 5.9 [5.7–6.8] | 2.5 [2.0–2.5] |
| get hit p99 µs | 1.5 [1.4–2.2] | 5.2 [4.2–6.5] | 10.2 [9.2–10.9] | 4.8 [3.7–6.7] |
| aggregate p50 ms | 0.23 [0.23–0.25] | 9.23 [8.02–9.30] | 9.35 [8.00–9.47] | 9.04 [8.08–9.31] |
| update commit p50 ms (100 docs) | 4.95 [4.93–4.97] | 6.06 [5.30–6.60] | 16.77 [15.96–21.04] | 0.77 [0.71–0.78] |
| delete commit p50 ms (100 docs) | 4.07 [4.03–4.92] | 5.10 [5.08–5.93] | 15.38 [15.08–16.89] | 0.61 [0.61–0.63] |
| reopen ms | 20.1 [19.3–21.9] | 0.5 [0.4–0.9] | 0.2 [0.1–0.2] | 0.4 [0.4–0.5] |
| disk MiB after load | 2.26 [2.26–2.26] | 2.45 [2.45–2.45] | 2.14 [2.14–2.14] | 2.45 [2.45–2.45] |
| disk MiB after changes | 2.55 [2.55–2.55] | 6.58 [6.58–6.58] | 2.34 [2.34–2.34] | 6.58 [6.58–6.58] |
| peak RSS MiB | 27.6 [21.8–27.8] | 11.6 [11.4–11.7] | 11.4 [11.4–11.4] | 11.5 [11.5–11.6] |

Durability contracts:
- cedrodb: append-only log; every commit calls File::sync_all (fsync, F_FULLFSYNC on macOS); cannot be disabled
- sqlite-wal-full: journal_mode=WAL, synchronous=FULL, fullfsync=ON (device-cache flush on macOS)
- sqlite-delete-full: journal_mode=DELETE, synchronous=FULL, fullfsync=ON (device-cache flush on macOS)
- sqlite-wal-normal: journal_mode=WAL, synchronous=NORMAL, fullfsync=ON (device-cache flush on macOS); NOT equivalent to CedroDB: a WAL commit is not synced until checkpoint

sqlite-wal-normal is not durability-equivalent to CedroDB; compare CedroDB with the *-full rows.
