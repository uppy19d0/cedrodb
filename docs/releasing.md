# Releasing an experimental version

Releases use the packages from a successful CI run, rather than rebuilding unrelated files immediately before upload. See [the roadmap](roadmap.md) for readiness gates beyond a successful build.

1. Implement one documented application capability, its failure behavior and meaningful tests. Add migration notes if an existing behavior changes.
2. Run `python3 scripts/set-version.py 0.x.y`. Update install examples, current README labels, changelog and use-case docs; historical migration references keep their original versions.
3. Run `python3 scripts/check-release-metadata.py`, Rust/SDK checks and the interoperability example. Commit the exact source to `main` or a `release/*` branch.
4. Wait for every CI job. The `distributions` artifact contains one npm tarball with four native targets, four Python ABI3 wheels, an sdist, the allowlisted source ZIP, SHA-256 files and the real Rust example output.
5. Check all artifact hashes and versions. Compare source ZIP entries against the release commit. Publish a GitHub release pinned to that commit and mark experimental releases accordingly.
6. Publish the tested tarball using an authenticated npm session. The manual `publish-registries.yml` workflow can publish the Python files from the successful run using a configured PyPI publisher. It does not rebuild them. Before either job runs, a `validate` job checks that `run_id` is a successful `ci.yml` push run on `main` in this repository and, for PyPI, that the sdist, six wheels (manylinux x86_64/aarch64, musllinux x86_64, macOS x86_64/arm64, Windows x64) and their `wheel-smoke-*` results exist; PyPI then publishes those wheels. These workflows are newly added and untested in Actions.
7. Wait for npm processing, check public registry metadata and compare downloaded files with CI hashes. Install into clean environments and run representative scenarios.
8. Set a registry flag in `cedrodb-artifacts.json` only after its exact version/files are available. Deploy the matching examples/downloads and verify gzip bytes/hashes through the production host.

Pushing tags does not repeat the build matrix: the release refers to a commit already validated before tagging. Existing source archives and immutable package versions are not replaced with binaries from a later run.

`package-portfolio.py` preserves verified registry flags when regenerating the same version; a new version starts with both flags false. Local builds may contain fewer native targets and are not a substitute for the release matrix.

Supported installations and limitations must match the actual artifacts. Stable 1.0 requires the broader compatibility, failure-injection, fuzzing and resource evidence in the roadmap; successfully uploading a pre-1.0 version does not meet those gates by itself.
