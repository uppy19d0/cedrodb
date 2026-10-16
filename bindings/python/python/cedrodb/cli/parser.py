"""Public command contracts; parsing completes before database handles are opened."""
import argparse
import os
from pathlib import Path

from .. import __version__
from .errors import CliUsageError
from .io import add_input, bounded_integer, finite_float, json_argument


class CliParser(argparse.ArgumentParser):
    def error(self, message):
        raise CliUsageError(message)


def create_parser():
    parser = CliParser(prog="cedrodb", description="CedroDB local database CLI. Run cedrodb COMMAND --help for details.", allow_abbrev=False)
    parser.add_argument("--version", action="version", version="cedrodb " + __version__)
    parser.add_argument("--json", action="store_true", help="compact JSON output, including errors")
    commands = parser.add_subparsers(dest="command", required=True)

    def command(name, help, database=True, aliases=()):
        sub = commands.add_parser(name, help=help, description=help, aliases=list(aliases), allow_abbrev=False)
        # Permit --json before or after the command without overriding the parent.
        sub.add_argument("--json", action="store_true", default=argparse.SUPPRESS, help="compact JSON output")
        if database:
            default = os.environ.get("CEDRODB_PATH")
            sub.add_argument("--db", type=Path, default=default, required=not default, help="database directory (or CEDRODB_PATH)")
        return sub

    command("doctor", "Show interpreter and native runtime details", database=False)
    bench = command("benchmark", "Measure an isolated synthetic workload", database=False)
    for flag, low, high, default in [("records", 10, 10000, 1000), ("iterations", 1, 1000, 30), ("batch-size", 1, 1000, 100), ("warmup", 0, 100, 3)]:
        bench.add_argument("--" + flag, type=bounded_integer(low, high), default=default)
    command("init", "Create a new database directory; refuse an existing database")
    for name, description in [("stats", "Show persisted storage counters"), ("diagnostics", "Show limits and session metrics"), ("collections", "List collection inventory without document values"), ("verify", "Verify the log after normal engine opening and recovery")]:
        command(name, description)
    for name in ["get", "put", "delete"]:
        sub = command(name, {"get": "Read a key, distinguishing missing records from JSON null", "put": "Write a JSON document", "delete": "Delete one document by key"}[name])
        sub.add_argument("collection")
        sub.add_argument("key")
        if name == "put": add_input(sub)
    batch = command("batch", "Atomically commit a JSON array of put/delete operations")
    add_input(batch)
    for name in ["find", "profile-query", "search", "profile-search"]:
        search = name in ("search", "profile-search")
        sub = command(name, "Search documents" if search else "Query a bounded page ordered by key", aliases=("query",) if name == "find" else ())
        sub.add_argument("collection")
        sub.add_argument("--filter", action="append", type=json_argument, default=[], help="repeat an AND filter as JSON {field,op,value}")
        sub.add_argument("--index", action="append", default=[], help="build an equality index for this invocation; repeatable")
        sub.add_argument("--limit", type=bounded_integer(1, 1000), default=10 if search else 100)
        if search:
            sub.add_argument("--text", help="BM25 text query")
            sub.add_argument("--vector", type=json_argument, help="JSON array of caller-supplied embedding components")
            sub.add_argument("--vector-field", default="embedding")
            sub.add_argument("--text-field", action="append", help="repeat for multiple indexed text fields")
            sub.add_argument("--max-scanned", type=bounded_integer(1, 100000), default=10000)
            sub.add_argument("--min-vector-score", type=finite_float)
        else:
            sub.add_argument("--after", help="exclusive keyset cursor from next_after")
    for name in ["backup", "restore"]:
        sub = command(name, "Create a verified backup file" if name == "backup" else "Restore a backup into a new directory")
        sub.add_argument("file", type=Path)
    changes = command("changes", "Read complete committed transactions after a cursor")
    changes.add_argument("--since", type=bounded_integer(0, 2**64 - 1), default=0)
    changes.add_argument("--limit", type=bounded_integer(1, 1000), default=100)
    return parser
