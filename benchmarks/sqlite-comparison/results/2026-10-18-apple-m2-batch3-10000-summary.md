CedroDB 0.8.0 vs SQLite 3.46.0 · release · Darwin 25.6.0 arm64 · Apple M2
records=10000 · 3 runs · median [min–max]

| metric | cedrodb | sqlite-wal-full | sqlite-delete-full | sqlite-wal-normal |
| --- | ---: | ---: | ---: | ---: |
| insert docs/s · 1 per commit | 212 [208–244] | 223 [219–230] | 71 [68–72] | 20,681 [20,246–24,139] |
| insert docs/s · 100 per commit | 19,119 [18,151–20,473] | 17,549 [16,295–17,607] | 6,912 [6,233–6,992] | 253,879 [250,325–265,917] |
| insert docs/s · 1000 per commit | 139,031 [138,819–156,821] | 139,918 [114,571–160,369] | 65,607 [45,260–66,110] | 625,674 [601,991–642,966] |
| commit p50 ms · 1 per commit | 4.13 [4.02–4.26] | 4.09 [4.08–4.11] | 13.75 [13.61–13.94] | 0.02 [0.02–0.02] |
| get hit p50 µs | 0.5 [0.4–0.5] | 2.5 [2.1–2.5] | 5.8 [5.4–6.9] | 2.5 [2.0–2.5] |
| get hit p99 µs | 1.8 [1.7–4.3] | 4.8 [4.5–9.3] | 9.4 [9.0–11.9] | 4.1 [4.1–4.4] |
| aggregate p50 ms | 0.24 [0.23–0.27] | 9.27 [8.35–9.34] | 9.28 [8.02–9.32] | 9.23 [8.18–9.33] |
| update commit p50 ms (100 docs) | 4.83 [4.58–4.93] | 5.27 [5.20–6.10] | 15.94 [15.08–19.76] | 0.72 [0.72–0.73] |
| delete commit p50 ms (100 docs) | 4.02 [4.01–4.83] | 5.07 [4.89–5.09] | 15.97 [15.38–17.16] | 0.59 [0.59–0.63] |
| reopen ms | 13.5 [12.8–14.7] | 0.4 [0.4–0.4] | 0.2 [0.1–0.2] | 0.4 [0.4–0.4] |
| disk MiB after load | 2.26 [2.26–2.26] | 2.45 [2.45–2.45] | 2.14 [2.14–2.14] | 2.45 [2.45–2.45] |
| disk MiB after changes | 2.55 [2.55–2.55] | 6.58 [6.58–6.58] | 2.34 [2.34–2.34] | 6.58 [6.58–6.58] |
| peak RSS MiB | 26.8 [26.7–27.0] | 11.7 [11.5–11.8] | 11.5 [11.5–11.7] | 11.7 [11.6–11.7] |

Durability contracts:
- cedrodb: append-only log; every commit calls File::sync_all (fsync, F_FULLFSYNC on macOS); cannot be disabled
- sqlite-wal-full: journal_mode=WAL, synchronous=FULL, fullfsync=ON (device-cache flush on macOS)
- sqlite-delete-full: journal_mode=DELETE, synchronous=FULL, fullfsync=ON (device-cache flush on macOS)
- sqlite-wal-normal: journal_mode=WAL, synchronous=NORMAL, fullfsync=ON (device-cache flush on macOS); NOT equivalent to CedroDB: a WAL commit is not synced until checkpoint

sqlite-wal-normal is not durability-equivalent to CedroDB; compare CedroDB with the *-full rows.
