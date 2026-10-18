# Releasing an experimental version

Releases use the packages from a successful CI run, rather than rebuilding unrelated files immediately before upload. See [the roadmap](roadmap.md) for readiness gates beyond a successful build.

1. Implement one documented application capability, its failure behavior and meaningful tests. Add migration notes if an existing behavior changes.
2. Run `python3 scripts/set-version.py 0.x.y`. Update install examples, current README labels, changelog and use-case docs; historical migration references keep their original versions.
3. Run `python3 scripts/check-release-metadata.py`, Rust/SDK checks and the interoperability example. Commit the exact source to `main` or a `release/*` branch.
4. Wait for every CI job. Two sets of files come out of CI and they are different things:
   - **Published** (what the registries receive): the `pywheel-*` matrix of six abi3 wheels (manylinux 2_28 x86_64 and aarch64, musllinux_1_2 x86_64, macOS x86_64 and arm64, Windows x64) and the `pysdist` sdist, all gated by the `wheel-smoke-*` jobs; and the npm tarball with four prebuilds (`darwin-arm64`, `darwin-x64`, `linux-x64-gnu`, `win32-x64-msvc`) built from the `node-*` artifacts.
   - **`distributions` artifact** (portfolio ZIP and site downloads): built by `package-portfolio.py` after the matrix, smoke and native jobs pass. It contains the npm tarball, the same six `pywheel-*` wheels and `pysdist` sdist that PyPI receives (the job fails unless there are exactly six wheels and one sdist), the allowlisted source ZIP, SHA-256 files and the real Rust example output. The native job's own `python-*` wheels are no longer used for distribution.
5. Check all artifact hashes and versions. Compare source ZIP entries against the release commit. Publish a GitHub release pinned to that commit and mark experimental releases accordingly.
6. Publish the tested tarball using an authenticated npm session. The manual `publish-registries.yml` workflow can publish the Python files from the successful run (the `pywheel-*` and `pysdist` artifacts) using a configured PyPI publisher. It does not rebuild them. Before either job runs, a `validate` job checks that `run_id` is a successful `ci.yml` push run on `main` in this repository and, for PyPI, that the sdist, six wheels (manylinux x86_64/aarch64, musllinux x86_64, macOS x86_64/arm64, Windows x64) their `wheel-smoke-*` results (plus the manylinux x86_64 Python 3.9 and 3.13 smokes) exist, and the npm job checks out the validated run's `head_sha`; PyPI then publishes those wheels. These workflows are newly added and untested in Actions.
7. Wait for npm processing, check public registry metadata and compare downloaded files with CI hashes. Install into clean environments and run representative scenarios.
8. Set a registry flag in `cedrodb-artifacts.json` only after its exact version/files are available. Deploy the matching examples/downloads and verify gzip bytes/hashes through the production host.

Pushing tags does not repeat the build matrix: the release refers to a commit already validated before tagging. Existing source archives and immutable package versions are not replaced with binaries from a later run.

`package-portfolio.py` preserves verified registry flags when regenerating the same version; a new version starts with both flags false. The portfolio's wheel filename parser (`installArtifacts.ts`) accepts only x86_64 manylinux, musllinux x86_64, macOS and `win_amd64` wheels, so the manylinux aarch64 wheel is listed in the raw manifest but never becomes an install command on the site until the portfolio is updated. Local builds may contain fewer native targets and are not a substitute for the release matrix.

Supported installations and limitations must match the actual artifacts. Stable 1.0 requires the broader compatibility, failure-injection, fuzzing and resource evidence in the roadmap; successfully uploading a pre-1.0 version does not meet those gates by itself.
