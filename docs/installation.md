# Install CedroDB on a compatible runtime

The installation page works on mobile and desktop screens. Running the native
SDK requires Node.js 18+ or standard CPython 3.9+ on a supported desktop/server
platform. Browser JavaScript, iOS and Android are outside the native SDK matrix.

## Choose language, operating system and interpreter architecture

| Runtime | Included native binaries |
| --- | --- |
| Node.js | macOS ARM64/x64, Linux x64 GNU/glibc, Windows x64 MSVC |
| Python | macOS ARM64 11+, macOS x86_64 (wheel tag 10.12+), Linux x86_64 glibc 2.34+, Windows x86_64 |

An emulated x64 runtime on an ARM64 machine needs an x64 binary. A browser's
user-agent does not reliably identify the architecture of the interpreter where
the package will be installed. Select the actual runtime, not the viewing device.
Other CPU/libc combinations have no included binary and are not part of the
tested release matrix. Free-threaded Python and PyPy are outside the tested ABI.

## Node.js / TypeScript

```sh
node --version
npm install cedrodb@0.7.0
node -e "console.log(require('cedrodb').version)"
```

The package selects an included native binary. Source fallback requires
Rust 1.88+, Cargo and a platform C linker. Source compilation is not a guarantee
that an untested target works. The Node GNU/Linux binary's minimum symbol
versions must be checked separately from Python's manylinux wheel tag.

## Python on macOS / Linux

```sh
python3 -m venv .venv
.venv/bin/python -m pip install cedrodb==0.7.0
.venv/bin/python -m cedrodb doctor
```

## Python on Windows PowerShell

```powershell
py -3 -m venv .venv
.\.venv\Scripts\python.exe -m pip install cedrodb==0.7.0
.\.venv\Scripts\python.exe -m cedrodb doctor
```

Activation is optional when you call the environment's interpreter directly.
Use the same interpreter for installation, verification and your application.
These commands follow Python's [virtual-environment guidance](https://docs.python.org/3/library/venv.html).

Pip chooses a compatible wheel when available. To avoid a source build:

```sh
python -m pip install --only-binary=:all: cedrodb==0.7.0
```

Python source builds require Rust 1.85+, Cargo and a C linker. Compatible wheels
need no Rust installation. macOS source builds need Xcode command-line tools;
Windows needs a compatible MSVC toolchain; Linux needs a compatible C toolchain.

## Verify and troubleshoot

`doctor` verifies that the Python native extension loads and prints the package,
interpreter, operating system, architecture and libc details without opening a
database. Node's version command loads its native package as well.

- **No compatible distribution:** check package version, interpreter ABI,
  architecture and the actual release assets.
- **Compiler/linker missing:** a source fallback was selected; install a
  compatible toolchain or choose a supported runtime with an included binary.
- **GLIBC version missing:** the Linux binary needs a newer compatible glibc
  runtime; Alpine/musl is a different target.
- **Database already open:** installation succeeded, but another owner holds
  the database. Close it before opening the same directory elsewhere.

Download packages and SHA256 checksums from [GitHub Releases](https://github.com/uppy19d0/cedrodb/releases).
The website lists actual artifacts and registry availability; it must not claim
that an unpublished package is installable by name. Historical releases remain
available. See [native SDK details](native-libraries.md) and
[performance tools](performance.md).
