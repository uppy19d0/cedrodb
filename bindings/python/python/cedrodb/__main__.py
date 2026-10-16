"""Local diagnostics and reproducible workloads: python -m cedrodb --help."""

import argparse
import json
import math
import platform
import sys
import time
from pathlib import Path
from tempfile import TemporaryDirectory

from . import Database, __version__


def _bounded_integer(low, high):
    def parse(value):
        number = int(value)
        if not low <= number <= high:
            raise argparse.ArgumentTypeError("must be in {}..{}".format(low, high))
        return number
    return parse


def environment():
    return {
        "cedrodb": __version__, "python": platform.python_version(),
        "implementation": platform.python_implementation(),
        "os": platform.system(), "os_release": platform.release(),
        "architecture": platform.machine(), "libc": list(platform.libc_ver()),
        "executable": sys.executable,
    }


def _latencies(values):
    ordered = sorted(values)
    def percentile(fraction):
        return ordered[max(0, math.ceil(len(ordered) * fraction) - 1)]
    return {
        "samples": len(ordered), "min": ordered[0], "p50": percentile(0.50),
        "p95": percentile(0.95), "p99": percentile(0.99), "max": ordered[-1],
        "mean": sum(ordered) / len(ordered),
    }


def _measure(call, iterations, warmup):
    for _ in range(warmup):
        call()
    native, sdk = [], []
    result = None
    for _ in range(iterations):
        started = time.perf_counter_ns()
        result = call()
        sdk.append((time.perf_counter_ns() - started) / 1000)
        native.append(result["elapsed_micros"])
    return {
        "native_latency_micros": _latencies(native),
        "sdk_latency_micros": _latencies(sdk), "plan": result["plan"],
    }, result["page"]


def benchmark(records=1000, iterations=30, batch_size=100, warmup=3):
    """Use a fresh temporary database; never open an application database."""
    if not 10 <= records <= 10_000 or not 1 <= iterations <= 1000:
        raise ValueError("records must be 10..10000 and iterations 1..1000")
    if not 1 <= batch_size <= 1000 or not 0 <= warmup <= 100:
        raise ValueError("batch_size must be 1..1000 and warmup 0..100")
    report = {
        "schema_version": 1, "environment": environment(),
        "workload": {
            "records": records, "iterations": iterations, "batch_size": batch_size,
            "warmup_per_phase": warmup, "tenants": 10, "vector_dimensions": 2,
            "vectors": "deterministic synthetic inputs; no model-quality evaluation",
            "durability": "sync_all for every nonempty committed batch",
            "percentile_method": "nearest rank; empirical samples, not a service SLO",
            "storage": "temporary directory; host filesystem and OS cache",
        },
    }
    with TemporaryDirectory(prefix="cedrodb-benchmark-") as directory:
        path = Path(directory) / "workload"
        with Database(path) as db:
            memory = db.collection("memory")
            batches = []
            for start in range(0, records, batch_size):
                values = {
                    "{:08d}".format(index): {
                        "tenant": "group:{}".format(index % 10),
                        "text": "local policy document",
                        "embedding": [1.0, (index % 7) / 7.0],
                    }
                    for index in range(start, min(start + batch_size, records))
                }
                started = time.perf_counter_ns()
                memory.put_many(values)
                batches.append((time.perf_counter_ns() - started) / 1000)
            report["batch_write_sdk_latency_micros"] = _latencies(batches)
            filters = [{"field": "tenant", "op": "eq", "value": "group:0"}]
            query = lambda: memory.profile_query(filters, limit=1000)
            search = lambda: memory.profile_search(
                text="local policy", vector=[1.0, 0.0], filters=filters,
                limit=10, max_scanned=records,
            )
            measurements = {}
            measurements["query_scan"], before_query = _measure(query, iterations, warmup)
            measurements["hybrid_scan"], before_search = _measure(search, iterations, warmup)
            started = time.perf_counter_ns()
            memory.create_index("tenant")
            report["index_build_sdk_micros"] = (time.perf_counter_ns() - started) / 1000
            measurements["query_indexed"], after_query = _measure(query, iterations, warmup)
            measurements["hybrid_indexed"], after_search = _measure(search, iterations, warmup)
            if before_query["records"] != after_query["records"]:
                raise RuntimeError("indexed query changed the returned records")
            if before_search["hits"] != after_search["hits"]:
                raise RuntimeError("indexed search changed ranking or scores")
            expected = (records + 9) // 10
            if len(after_query["records"]) != expected:
                raise RuntimeError("query returned an unexpected record count")
            if len(after_search["hits"]) != min(10, expected):
                raise RuntimeError("search returned an unexpected hit count")
            report["measurements"] = measurements
            report["diagnostics"] = db.diagnostics()
            report["integrity"] = db.verify()
            report["correctness"] = {"query_equal": True, "search_equal": True}
        started = time.perf_counter_ns()
        reopened = Database(path)
        report["reopen_sdk_micros"] = (time.perf_counter_ns() - started) / 1000
        with reopened:
            if reopened.stats()["records"] != records:
                raise RuntimeError("reopen changed the record count")
            report["reopened_diagnostics"] = reopened.diagnostics()
        report["correctness"]["reopen_record_count_equal"] = True
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("doctor", help="print runtime details after loading the native extension")
    bench = commands.add_parser("benchmark", help="measure an isolated synthetic workload")
    bench.add_argument("--records", type=_bounded_integer(10, 10_000), default=1000)
    bench.add_argument("--iterations", type=_bounded_integer(1, 1000), default=30)
    bench.add_argument("--batch-size", type=_bounded_integer(1, 1000), default=100)
    bench.add_argument("--warmup", type=_bounded_integer(0, 100), default=3)
    args = parser.parse_args(argv)
    if args.command == "doctor":
        result = {"native_extension_loaded": True, "environment": environment()}
    else:
        result = benchmark(args.records, args.iterations, args.batch_size, args.warmup)
    print(json.dumps(result, ensure_ascii=True, allow_nan=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
