# CedroDB CLI

The Python distribution now registers the `cedrodb` executable. It is included from 0.8.0; previously published 0.7.0 artifacts contain only the older `python -m cedrodb doctor` and `benchmark` commands.

Install the 0.8.0 Python package or build its wheel from this checkout:

```sh
python -m pip install cedrodb==0.8.0
cedrodb --help
cedrodb --version
cedrodb init --db ./local-data
```

`python -m cedrodb` exposes the same interface. The CLI uses the native Rust engine through the Python SDK; it does not start a server or call an embedding provider. Node-only installations do not install this executable.

## Commands

| Command | Purpose |
| --- | --- |
| `init` | Create a new database; refuse a nonempty target |
| `put COLLECTION KEY` | Write one JSON document |
| `get COLLECTION KEY` | Read one key, distinguishing absence from null |
| `delete COLLECTION KEY` | Delete a document; repeated deletion succeeds with `deleted:false` |
| `batch` | Atomically apply a JSON array of put/delete operations |
| `find COLLECTION` / `query COLLECTION` | Read a bounded keyset page |
| `search COLLECTION` | Text, exact vector or hybrid retrieval |
| `profile-query COLLECTION` | Query page, plan and native elapsed microseconds |
| `profile-search COLLECTION` | Search page, plan and native elapsed microseconds |
| `collections` | Collection names and counts, without document values |
| `stats` | Persistent counters |
| `diagnostics` | Inventory, limits and handle-local metrics |
| `verify` | Verify the committed log after normal database opening |
| `backup FILE` | Write a new verified full-history backup file |
| `restore FILE` | Restore into a directory that does not exist |
| `changes` | Read complete transactions after an exclusive cursor |
| `doctor` | Interpreter, architecture and native runtime details |
| `benchmark` | Isolated synthetic workload in a temporary directory |

Database commands require `--db PATH`, or `CEDRODB_PATH`. Only `init` creates an application database; a mistyped path on an existing-database command returns an error. `restore` requires a new destination. Each invocation holds the engine's exclusive lock and closes the handle on exit.

## JSON input and output

Successful data output is JSON, indented by default. `--json`, before or after the command, emits compact JSON and formats errors as JSON on stderr. Help and version remain ordinary text. stdout contains no diagnostic banners.

`put` and `batch` require exactly one of `--value JSON`, `--file PATH`, or `--stdin`. Prefer files or stdin when values should not appear in shell history or process arguments. Input is limited to 16 MiB, decoded as strict UTF-8 JSON, and rejects duplicate object keys and nonfinite numbers. Engine document and transaction limits also apply.

```sh
printf '%s' '{"name":"Luis","active":true}' | cedrodb put users luis --db ./local-data --stdin
cedrodb get users luis --db ./local-data --json
cedrodb put users nullable --db ./local-data --value 'null'
cedrodb put users from-file --db ./local-data --file ./user.json
```

An existing JSON null returns `{"found":true,"key":"nullable","value":null}`. An absent record returns `{"found":false,"key":"missing"}` and exit code 3. Batch input uses the same `{kind,collection,key,value}` records as the SDK; deletes omit `value`. An invalid batch commits no operations.

## Querying and retrieval

```sh
cedrodb find users --db ./local-data --limit 20 --filter '{"field":"active","op":"eq","value":true}' --index active
cedrodb find users --db ./local-data --after luis --limit 20
cedrodb search memory --db ./local-data --text 'refund policy' --text-field title --text-field text
cedrodb profile-query users --db ./local-data --index active --filter '{"field":"active","op":"eq","value":true}'
```

Repeat `--filter` to AND conditions. `next_after` is an exclusive keyset cursor, not a snapshot token. Repeat `--index` to create session-local equality indexes for that invocation; indexes are not persisted between CLI processes.

`search` and `profile-search` accept `--vector` as a JSON array, `--vector-field`, `--text-field`, `--max-scanned` and `--min-vector-score`. Supply real embeddings from your application. Filter by embedding model/version and tenant before comparing vectors; equal dimensions do not imply compatibility. Add `--text` to a vector query for hybrid retrieval. The CLI does not generate embeddings. Exceeding the scan budget returns an error, not partial results.

## Backups and operations

```sh
cedrodb backup ./snapshot.cedrobak --db ./local-data
cedrodb restore ./snapshot.cedrobak --db ./restored-data
cedrodb verify --db ./restored-data
cedrodb changes --db ./local-data --since 0 --limit 100
cedrodb diagnostics --db ./local-data
```

Existing backup files and restore destinations are never overwritten. Opening uses the engine's usual recovery rules, which can discard an incomplete log tail; `verify` is not a forensic open mode. Deletion is logical: transaction history remains in logs and backups. Compaction, encryption and automatic retention are not added by the CLI.

| Exit code | Meaning |
| --- | --- |
| 0 | Success, including idempotent deletion |
| 1 | Engine or filesystem operation failed |
| 2 | Usage, input or argument validation failed |
| 3 | `get` did not find the requested key |
| 130 | Interrupted |

## Build from the checkout

Use a virtual environment with maturin and a compatible Rust toolchain:

```sh
python -m venv .venv
# Activate the environment using your shell's activation command.
python -m pip install 'maturin>=1.15,<2'
python bindings/python/scripts/prepare_core.py
maturin develop --release --locked --manifest-path bindings/python/Cargo.toml
cedrodb --help
python -m unittest discover -s bindings/python/tests -v
```

The implementation is separated into `cedrodb/cli/parser.py`, `io.py`, `database.py`, `documents.py` and `performance.py`, with a thin `__main__.py`. Console and module execution share the same implementation. Tests use subprocesses and real native databases, including pagination, atomic failures, lock handling, hybrid search, snapshots and restoration.
