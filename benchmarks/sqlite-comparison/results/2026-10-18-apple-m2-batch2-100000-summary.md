CedroDB 0.8.0 vs SQLite 3.46.0 · release · Darwin 25.6.0 arm64 · Apple M2
records=100000 · 3 runs · median [min–max]

| metric | cedrodb | sqlite-wal-full | sqlite-delete-full | sqlite-wal-normal |
| --- | ---: | ---: | ---: | ---: |
| insert docs/s · 1 per commit | 238 [226–245] | 229 [224–232] | 67 [63–70] | 21,450 [20,282–21,980] |
| insert docs/s · 100 per commit | 19,537 [17,988–19,702] | 18,209 [17,862–19,345] | 6,490 [6,236–6,577] | 256,338 [239,061–259,734] |
| insert docs/s · 1000 per commit | 144,327 [138,195–145,755] | 140,767 [134,860–144,849] | 58,951 [50,546–61,789] | 358,261 [327,641–366,626] |
| commit p50 ms · 1 per commit | 4.05 [4.05–4.08] | 4.08 [4.06–4.09] | 14.20 [14.02–14.92] | 0.01 [0.01–0.02] |
| get hit p50 µs | 0.8 [0.8–0.8] | 3.9 [3.8–3.9] | 7.0 [6.2–7.7] | 3.7 [3.4–4.6] |
| get hit p99 µs | 2.2 [1.8–2.3] | 6.0 [5.8–6.2] | 13.0 [10.6–36.8] | 7.2 [5.9–37.3] |
| aggregate p50 ms | 6.88 [6.44–7.14] | 97.09 [92.59–97.24] | 97.06 [83.32–102.53] | 96.51 [83.29–97.17] |
| update commit p50 ms (100 docs) | 4.95 [4.92–5.12] | 5.92 [5.85–5.95] | 20.03 [18.92–21.09] | 1.06 [1.04–1.08] |
| delete commit p50 ms (100 docs) | 4.94 [4.89–5.00] | 5.87 [5.11–5.98] | 21.04 [19.98–23.69] | 0.90 [0.87–0.94] |
| reopen ms | 175.8 [167.0–186.6] | 0.4 [0.4–0.4] | 0.1 [0.1–0.2] | 0.4 [0.4–0.5] |
| disk MiB after load | 22.89 [22.89–22.89] | 25.18 [25.18–25.18] | 22.30 [22.30–22.30] | 25.18 [25.18–25.18] |
| disk MiB after changes | 25.79 [25.79–25.79] | 29.02 [29.02–29.02] | 24.26 [24.26–24.26] | 29.02 [29.02–29.02] |
| peak RSS MiB | 133.1 [132.1–133.4] | 13.5 [13.5–13.7] | 13.4 [12.4–13.5] | 13.7 [13.6–13.7] |

Durability contracts:
- cedrodb: append-only log; every commit calls File::sync_all (fsync, F_FULLFSYNC on macOS); cannot be disabled
- sqlite-wal-full: journal_mode=WAL, synchronous=FULL, fullfsync=ON (device-cache flush on macOS)
- sqlite-delete-full: journal_mode=DELETE, synchronous=FULL, fullfsync=ON (device-cache flush on macOS)
- sqlite-wal-normal: journal_mode=WAL, synchronous=NORMAL, fullfsync=ON (device-cache flush on macOS); NOT equivalent to CedroDB: a WAL commit is not synced until checkpoint

sqlite-wal-normal is not durability-equivalent to CedroDB; compare CedroDB with the *-full rows.
