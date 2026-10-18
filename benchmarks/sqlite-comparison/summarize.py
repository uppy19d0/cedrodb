#!/usr/bin/env python3
"""Summarize repeated comparison runs: median and min–max across runs for every engine.

Usage: python3 summarize.py results/2026-10-18-apple-m2-100000-run*.json
Only runs whose engines all report `verified: true` are accepted.
"""
import json
import statistics
import sys

METRICS = [
    ("insert docs/s · 1 per commit", lambda e: e["inserts"][0]["documents_per_second"], 1, 0),
    ("insert docs/s · 100 per commit", lambda e: e["inserts"][1]["documents_per_second"], 1, 0),
    ("insert docs/s · 1000 per commit", lambda e: e["inserts"][2]["documents_per_second"], 1, 0),
    ("commit p50 ms · 1 per commit", lambda e: e["inserts"][0]["commit_latency"]["p50_us"], 1000, 2),
    ("get hit p50 µs", lambda e: e["get_hit"]["p50_us"], 1, 1),
    ("get hit p99 µs", lambda e: e["get_hit"]["p99_us"], 1, 1),
    ("aggregate p50 ms", lambda e: e["aggregate"]["p50_us"], 1000, 2),
    ("update commit p50 ms (100 docs)", lambda e: e["update_10_percent"]["commit_latency"]["p50_us"], 1000, 2),
    ("delete commit p50 ms (100 docs)", lambda e: e["delete_10_percent"]["commit_latency"]["p50_us"], 1000, 2),
    ("reopen ms", lambda e: e["reopen_ms"], 1, 1),
    ("disk MiB after load", lambda e: e["disk_bytes_after_load"], 1048576, 2),
    ("disk MiB after changes", lambda e: e["disk_bytes_after_changes"], 1048576, 2),
    ("peak RSS MiB", lambda e: e["peak_rss_kib"], 1024, 1),
]


def main(paths):
    runs = [json.load(open(path)) for path in paths]
    if not runs:
        sys.exit("no result files given")
    for path, run in zip(paths, runs):
        bad = [e["engine"] for e in run["engines"] if not e["verified"]]
        if bad:
            sys.exit(f"{path}: verification failed for {', '.join(bad)}; discard this run")
    first = runs[0]
    names = [e["engine"] for e in first["engines"]]
    env = first["environment"]
    print(f"CedroDB {env['cedrodb_version']} vs SQLite {env['sqlite_version']} · {env['build']} · {env['os']} · {env['cpu']}")
    print(f"records={first['parameters']['records']} · {len(runs)} runs · median [min–max]\n")
    print("| metric | " + " | ".join(names) + " |")
    print("| --- | " + " | ".join("---:" for _ in names) + " |")
    for label, getter, scale, decimals in METRICS:
        cells = []
        for index, name in enumerate(names):
            values = [getter(run["engines"][index]) / scale for run in runs]
            fmt = f"{{:,.{decimals}f}}"
            cells.append(f"{fmt.format(statistics.median(values))} [{fmt.format(min(values))}–{fmt.format(max(values))}]")
        print(f"| {label} | " + " | ".join(cells) + " |")
    print("\nDurability contracts:")
    for engine in first["engines"]:
        print(f"- {engine['engine']}: {engine['durability']}")
    print("\nsqlite-wal-normal is not durability-equivalent to CedroDB; compare CedroDB with the *-full rows.")


main(sys.argv[1:])
