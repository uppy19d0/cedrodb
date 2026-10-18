CedroDB 0.8.0 vs SQLite 3.46.0 · release · Darwin 25.6.0 arm64 · Apple M2
records=100000 · 3 runs · median [min–max]

| metric | cedrodb | sqlite-wal-full | sqlite-delete-full | sqlite-wal-normal |
| --- | ---: | ---: | ---: | ---: |
| insert docs/s · 1 per commit | 242 [234–243] | 227 [217–233] | 70 [68–70] | 21,065 [20,262–21,863] |
| insert docs/s · 100 per commit | 20,034 [19,776–20,343] | 18,276 [17,924–19,486] | 6,570 [6,418–6,608] | 242,137 [235,428–264,768] |
| insert docs/s · 1000 per commit | 147,773 [142,018–156,463] | 138,073 [137,860–146,228] | 57,855 [55,212–58,127] | 365,115 [333,709–365,298] |
| commit p50 ms · 1 per commit | 4.05 [4.04–4.06] | 4.07 [4.06–4.10] | 13.96 [13.83–13.97] | 0.01 [0.01–0.01] |
| get hit p50 µs | 0.8 [0.8–0.9] | 3.7 [3.3–4.0] | 7.2 [6.9–7.2] | 3.8 [3.5–3.8] |
| get hit p99 µs | 2.3 [2.3–2.5] | 6.0 [4.7–6.3] | 11.7 [10.4–41.2] | 7.2 [4.9–23.1] |
| aggregate p50 ms | 6.91 [6.60–7.09] | 96.87 [84.98–97.32] | 97.19 [96.73–97.86] | 84.46 [83.56–96.95] |
| update commit p50 ms (100 docs) | 4.94 [4.82–5.01] | 6.06 [5.99–6.98] | 20.00 [19.55–20.89] | 1.08 [1.05–1.08] |
| delete commit p50 ms (100 docs) | 4.91 [4.90–5.01] | 5.14 [5.13–7.56] | 20.95 [20.76–21.54] | 0.90 [0.87–0.96] |
| reopen ms | 101.7 [97.6–111.9] | 0.4 [0.4–0.4] | 0.2 [0.1–0.2] | 0.4 [0.4–0.5] |
| disk MiB after load | 22.89 [22.89–22.89] | 25.18 [25.18–25.18] | 22.30 [22.30–22.30] | 25.18 [25.18–25.18] |
| disk MiB after changes | 25.79 [25.79–25.79] | 29.02 [29.02–29.02] | 24.26 [24.26–24.26] | 29.02 [29.02–29.02] |
| peak RSS MiB | 140.9 [140.1–143.5] | 13.6 [13.5–13.8] | 13.5 [13.5–13.6] | 13.7 [13.7–13.7] |

Durability contracts:
- cedrodb: append-only log; every commit calls File::sync_all (fsync, F_FULLFSYNC on macOS); cannot be disabled
- sqlite-wal-full: journal_mode=WAL, synchronous=FULL, fullfsync=ON (device-cache flush on macOS)
- sqlite-delete-full: journal_mode=DELETE, synchronous=FULL, fullfsync=ON (device-cache flush on macOS)
- sqlite-wal-normal: journal_mode=WAL, synchronous=NORMAL, fullfsync=ON (device-cache flush on macOS); NOT equivalent to CedroDB: a WAL commit is not synced until checkpoint

sqlite-wal-normal is not durability-equivalent to CedroDB; compare CedroDB with the *-full rows.
