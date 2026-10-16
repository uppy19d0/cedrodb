"""Database lifecycle and maintenance commands."""
from pathlib import Path

from .. import Database
from .errors import CliUsageError


def open_database(args):
    directory = Path(args.db)
    if args.command == "init":
        if directory.exists() and (not directory.is_dir() or any(directory.iterdir())):
            raise CliUsageError("init requires a new or empty directory")
    elif not (directory / "cedro.log").is_file():
        raise CliUsageError("database does not exist; run cedrodb init --db PATH first")
    return Database(directory)


def maintenance(db, args):
    if args.command == "init":
        return {"created": True, "path": str(Path(args.db).resolve()), "version": db.stats()["version"]}
    if args.command == "stats":
        return db.stats()
    if args.command == "diagnostics":
        return db.diagnostics()
    if args.command == "collections":
        return {"collections": db.diagnostics()["collections"]}
    if args.command == "verify":
        return db.verify()
    if args.command == "backup":
        return db.backup(args.file)
    if args.command == "changes":
        return db.changes(since=args.since, limit=args.limit)
    raise CliUsageError("unknown maintenance command")
