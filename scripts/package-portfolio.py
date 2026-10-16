#!/usr/bin/env python3
"""Run the real Rust demo and package an allowlisted source download."""

import argparse
import hashlib
import io
import json
import pathlib
import shutil
import subprocess
import tarfile
import zipfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--portfolio", type=pathlib.Path, required=True)
    parser.add_argument("--sdk-dir", type=pathlib.Path, help="Directory containing the tested npm/Python distributions")
    arguments = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parents[1]
    # Cargo.toml is controlled source; Cargo is the authority for package metadata.
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], cwd=root
    ))
    package = next(item for item in metadata["packages"] if item["name"] == "cedrodb")
    version = package["version"]
    raw = subprocess.check_output(
        ["cargo", "run", "--locked", "--quiet", "--example", "portfolio_demo"], cwd=root
    )
    fixture = json.loads(raw)
    expected = {"field": "amount", "count": 3, "sum": 450.0, "min": 74.5, "max": 250.0}
    if fixture["engine"] != {"name": "CedroDB", "version": version}:
        raise ValueError("Demo engine metadata does not match Cargo package")
    if fixture["aggregate"] != expected or len(fixture["records"]) != 3:
        raise ValueError("Unexpected Rust demo results; review the public example")
    if [item["key"] for item in fixture["records"]] != ["order:001", "order:002", "order:003"]:
        raise ValueError("Unexpected order keys")
    advanced = fixture["advanced"]
    if advanced["query"]["used_index"] != "status" or [record["key"] for record in advanced["query"]["records"]] != ["order:001"]:
        raise ValueError("Unexpected indexed query result")
    if advanced["compare_exchange"] != {"matched": True, "mismatch_rejected": True}:
        raise ValueError("Unexpected conditional update result")
    if advanced["changes"]["next_cursor"] != 2 or len(advanced["changes"]["transactions"]) != 2:
        raise ValueError("Unexpected durable changefeed result")
    cases = fixture["use_cases"]
    if not cases["maintenance"]["cursors_preserved"] or cases["maintenance"]["verified"] != cases["maintenance"]["restored"]:
        raise ValueError("Backup restoration did not preserve verified history")
    inventory = cases["inventory"]
    if not all(inventory[name] for name in ["reserved", "duplicate_rejected", "stale_rejected"]) or inventory["remaining"] != 0 or inventory["orders"] != 1 or inventory["outbox_events"] != 1:
        raise ValueError("Guarded stock reservation produced unexpected results")
    jobs = cases["jobs"]
    if not all(jobs[name] for name in ["deduplicated", "expired_reclaimed", "stale_ack_rejected", "completed", "dead_lettered", "history_resumed"]) or jobs["attempts"] != 2 or jobs["outputs"] != 1 or jobs["stats"]["done"] != 1 or jobs["stats"]["dead"] != 1:
        raise ValueError("Durable queue recovery produced unexpected results")
    agent = cases["agent_memory"]
    if not all(agent[name] is True for name in ["checkpoint_saved", "stale_rejected", "resumed", "atomic_outbox"]) or agent["search_mode"] != "hybrid" or agent["search_hits"] != 2 or agent["scanned"] != 2 or agent["top_key"] != "refund-policy" or agent["revision"] != 2:
        raise ValueError("Agent checkpoint and hybrid-memory recovery produced unexpected results")
    performance = cases["performance"]
    if performance["results_equal"] is not True:
        raise ValueError("Profiling changed the query/search results")
    for operation in ["query", "search"]:
        scan, indexed = performance[operation + "_scan"], performance[operation + "_indexed"]
        if scan["scanned"] != 100 or indexed["scanned"] != 10 or indexed["used_index"] != "tenant":
            raise ValueError("Index diagnostics do not describe the executed workload")
    session = performance["diagnostics"]["session"]
    if session["query"]["successes"] != 2 or session["search"]["successes"] != 2 or session["commit"]["successes"] != 1:
        raise ValueError("Session diagnostics do not count the executed workload")

    fixed = ["Cargo.toml", "Cargo.lock", "README.md", "CHANGELOG.md", "LICENSE", ".gitignore"]
    sources = [root / name for name in fixed]
    for directory in ["src", "examples", "tests", "docs", "scripts", ".github", "bindings"]:
        sources.extend(path for path in (root / directory).rglob("*") if path.is_file())
    excluded = {"target", "node_modules", "__pycache__", "core", "dist", "prebuilds", ".venv"}
    sources = [path for path in sources if not (set(path.relative_to(root).parts) & excluded)
               and path.suffix not in {".pyc", ".tgz", ".whl", ".node"}]
    archive_data = io.BytesIO()
    with zipfile.ZipFile(archive_data, mode="w", compression=zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(sources):
            if path.is_symlink():
                raise ValueError("Source archive must not include symlinks")
            entry = zipfile.ZipInfo("cedrodb-" + version + "/" + path.relative_to(root).as_posix())
            entry.create_system = 3
            entry.external_attr = 0o100644 << 16
            archive.writestr(entry, path.read_bytes(), compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)

    public = arguments.portfolio.resolve() / "public"
    demos = public / "demos"
    downloads = public / "downloads"
    demos.mkdir(parents=True, exist_ok=True)
    downloads.mkdir(parents=True, exist_ok=True)
    filename = "cedrodb-" + version + ".zip"
    content = archive_data.getvalue()
    (downloads / filename).write_bytes(content)
    digest = hashlib.sha256(content).hexdigest()
    (downloads / (filename + ".sha256")).write_text(digest + "  " + filename + "\n")
    (demos / "cedrodb-results.json").write_text(
        json.dumps(fixture, ensure_ascii=False, indent=2, allow_nan=False) + "\n"
    )
    artifacts = []
    platforms = []
    if arguments.sdk_dir:
        for path in sorted(arguments.sdk_dir.resolve().glob("cedrodb-" + version + "*")):
            if not path.is_file() or not (path.name.endswith((".whl", ".tgz", ".tar.gz"))):
                continue
            destination = downloads / path.name
            if destination.resolve() != path.resolve():
                shutil.copy2(path, destination)
            checksum = hashlib.sha256(destination.read_bytes()).hexdigest()
            (downloads / (path.name + ".sha256")).write_text(checksum + "  " + path.name + "\n")
            artifacts.append({"filename": path.name, "sha256": checksum, "bytes": destination.stat().st_size,
                              "url": "https://www.ltavarez.me/downloads/" + path.name})
            if path.suffix == ".tgz":
                with tarfile.open(path, mode="r:gz") as package:
                    platforms = sorted({pathlib.PurePosixPath(name).parent.name for name in package.getnames()
                                        if name.endswith("/cedrodb.node") and "/prebuilds/" in name})
    registries = {"npm": False, "pypi": False}
    previous_manifest = demos / "cedrodb-artifacts.json"
    if previous_manifest.exists():
        previous = json.loads(previous_manifest.read_text())
        if previous.get("version") == version:
            registries = {name: previous.get("registries", {}).get(name) is True for name in registries}
    manifest = {"version": version, "registries": registries,
                "node_prebuilds": platforms, "artifacts": artifacts}
    (demos / "cedrodb-artifacts.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps({"fixture": str(demos / "cedrodb-results.json"), "archive": str(downloads / filename),
                      "sha256": digest, "bytes": len(content)}, indent=2))


if __name__ == "__main__":
    main()
