"""CedroDB CLI: console entry point and consistent output/error handling."""
import json
import sys

from .. import CedroError, Database
from .database import maintenance, open_database
from .documents import execute
from .errors import CliUsageError
from .io import input_json
from .parser import create_parser
from .performance import benchmark, environment

DOCUMENT_COMMANDS = {"put", "get", "delete", "batch", "find", "query", "profile-query", "search", "profile-search"}


def _emit(value, compact, stream):
    text = json.dumps(value, ensure_ascii=True, allow_nan=False,
                      indent=None if compact else 2,
                      separators=(",", ":") if compact else None)
    stream.write(text + "\n")


def main(argv=None):
    arguments = sys.argv[1:] if argv is None else list(argv)
    compact = "--json" in arguments
    try:
        args = create_parser().parse_args(arguments)
        compact = args.json
        # Read and validate JSON before opening any application database.
        payload = input_json(args) if args.command in ("put", "batch") else None
        if args.command == "doctor":
            result, status = {"native_extension_loaded": True, "environment": environment()}, 0
        elif args.command == "benchmark":
            result, status = benchmark(args.records, args.iterations, args.batch_size, args.warmup), 0
        elif args.command == "restore":
            result, status = Database.restore_backup(args.file, args.db), 0
        else:
            with open_database(args) as db:
                if args.command in DOCUMENT_COMMANDS:
                    result, status = execute(db, args, payload)
                else:
                    result, status = maintenance(db, args), 0
        _emit(result, compact, sys.stdout)
        return status
    except BrokenPipeError:
        # A consumer such as head may stop reading before all JSON is written.
        return 0
    except (CedroError, OSError, ValueError, TypeError, RecursionError) as error:
        status = 2 if isinstance(error, (ValueError, TypeError, RecursionError)) else 1
        if compact:
            _emit({"error": {"code": "invalid_arguments" if status == 2 else "operation_failed", "message": str(error)}}, True, sys.stderr)
        else:
            sys.stderr.write("cedrodb: {}\n".format(error))
        return status
    except KeyboardInterrupt:
        return 130
