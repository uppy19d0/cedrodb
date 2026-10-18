# CedroDB production-readiness audit

Status: audit of branch `audit/production-readiness` (Cargo version 0.8.0).
Method: static reading of source, tests, docs and CI by several auditors, with each defect re-checked by three independent verifiers. **No auditor ran the test suite, benchmarks, fault injection or a power-loss experiment.** Every statement below is from code reading unless it says otherwise. Where verifiers disagreed, this is stated. Line numbers come from the audit data; verifiers noted a few stale citations, which are flagged.

## 1. Executive summary

**Verdict.** CedroDB is a carefully built, honestly documented experimental embedded store. Its crash-recovery logic (torn-tail truncation, hard failure on complete corrupt frames, poison-on-I/O-error) is sound by reading and well covered by deterministic tests. It is **not validated for production or a stable 1.0**, and the project says so itself (README.md:9, docs/roadmap.md:17-28). The gaps are validation evidence (no fuzzing, no I/O fault injection, no historical format fixtures, no power-loss evidence) and unbounded growth (no compaction). The audit found no memory-safety problem and no confirmed silent-data-loss defect.

Five most important facts:

1. **Single write path, fsync before apply.** All writers go through `commit_operations`. A frame is written with `write_all` + `sync_all` and only then applied to memory (src/lib.rs:571-606). Any I/O error poisons the handle (lib.rs:586-604).
2. **Recovery is strict and tested.** A short tail is truncated; a complete frame with a bad length complement, CRC or content is a hard `Corruption` error (lib.rs:865-925). Truncation at every byte boundary is tested (tests/durability.rs:121).
3. **Validation evidence is thin.** The only "crash" test uses `process::exit(0)` after commit (tests/durability.rs:263-294). No SIGKILL, fault injection, fuzzing, property tests or released-version fixtures exist. The only I/O-failure test uses a read-only descriptor (lib.rs:952).
4. **The log and RAM grow without bound.** There is no compaction; open replays the whole log (201.9 ms for 100k docs, per the benchmark docs), 8 bytes of RAM per transaction, and done/dead jobs are never pruned. Cursors, checkpoint revisions and lease tokens all derive from frame ordinals, so compaction needs an RFC.
5. **One coarse mutex serializes everything.** Reads block during fsync, backup, verify and long scans (by design, documented). The one-shot `put`/`delete` helpers can spuriously return `WriterBusy` under thread contention (src/advanced.rs:62-72).

## 2. Current architecture assessment

- **Storage format.** One append-only file `cedro.log`. 16-byte header (`CEDRODB\0`, version byte 0x01, padding; lib.rs:34). Each commit is a frame: u32 LE length, inverted length, CRC32, JSON payload `{"ops":[...]}` (lib.rs:571-590). CRC32 is an accidental-corruption check only. The header is an exact 16-byte match; any other version is rejected as generic `Corruption`. The format is documented as experimental (docs/architecture.md); no formal spec or fixtures.
- **Write path.** Validate and plan in memory (`prepare_changes`), serialize, seek to EOF, `write_all`, `sync_all`, then `apply_changes` and push the frame offset (lib.rs:571-606). Creation syncs the file, then the directory and its parent on Unix; directory sync is a no-op elsewhere (lib.rs:391-397, 934-943; untested).
- **Recovery.** `recover` (lib.rs:865-925) replays all frames. Short tail: truncated and fsynced. Everything else invalid: `Corruption`, file untouched. Limits come from the caller's `Options`, not the file (see D1).
- **Memory model.** Entire live state in `BTreeMap<String, BTreeMap<String, Document>>` of `serde_json::Value` (lib.rs:156-163). `max_state_bytes` (default 64 MiB) counts encoded content only; it is documented as not an RSS bound (lib.rs:100-105; docs/benchmarks.md gives roughly 2 to 2.5x on one workload, unmeasured elsewhere). Indexes (max 64, equality only, handle-local) and `frame_offsets` are extra. `get`/`scan`/`query`/`search` clone under the lock.
- **Concurrency model.** One `Mutex<Inner>` (lib.rs:177-181, 418-424). One staged writer slot (`writer_active`); contenders fail fast with `WriterBusy`. Staging happens outside the mutex; commit (including fsync) holds it. Exclusive advisory `fs2` lock on `cedro.log` gives `DatabaseLocked` for a second handle or process. No snapshots or MVCC; pages and calls do not share a view (README.md:165).
- **Search.** Exact, in-memory: BM25, exact cosine, RRF hybrid (src/search.rs). Caller supplies embeddings; no network or model code. `max_scanned` (default 10,000, max 100,000) is a hard error on overflow, never a partial top-k. BM25 statistics are computed over the filtered set (documented, docs/agents.md:104). Per-query cost O(n*d + n log n) under the mutex.
- **Jobs and agents.** Queue = JSON documents in `_cedro.jobs.<name>`, each transition one fsynced frame; claim is an O(n) scan; leases fenced by token `q1:{cursor}:{attempts}:{id}`; all time is caller-supplied `now_ms`. `ack` commits Done plus caller business/outbox writes in one frame. The outbox is an ordinary collection; there is no dispatcher. Agent checkpoints use the global cursor as revision for compare-and-set. Delivery is at-least-once; docs disclaim exactly-once.
- **Backup.** 32-byte container header (magic, u64 length, CRC32) plus the whole log (src/maintenance.rs). Holds the mutex through verify, copy and sync; rejects an active writer. `create_new` never overwrites; restore requires a nonexistent destination and replays the log first. Backup now publishes via temp file, sync, no-replace hard link and directory sync (D4 addressed; tested for error cleanup and refusal, not under real process kills). Restore now builds in a sibling `.restoring` directory and renames it into place (D5 addressed; tested for no partial state at the final name, not under real process kills).
- **Bindings.** Node (napi-rs) and Python (pyo3) are JSON-string bridges over the same engine. Node rejects numbers beyond 2^53-1 on read and write and reports untyped errors; Python allows up to 2^64-1 and has `CedroError`/`DatabaseClosedError`. Python releases the GIL and serializes with a `Mutex<Option<Database>>`; Node is synchronous. `bindings/*/core` are gitignored build products regenerated by CI.

## 3. Guarantees the code provides today

| # | Guarantee | Evidence | Status |
|---|---|---|---|
| G1 | Memory changes only after the whole frame is written and `sync_all` returned Ok; readers never see a failed/uncommitted batch | lib.rs:586-606; tests/durability.rs:74,98 | tested |
| G2 | A torn tail (partial header or short payload) is truncated on reopen and fsynced; preceding commits survive | lib.rs:877-880, 907-910, 927-931; tests/durability.rs:121,170 | tested |
| G3 | A complete corrupt frame (CRC) fails open and leaves the file unmodified | lib.rs:914-918; tests/durability.rs:185 | tested |
| G4 | Inverted-length copy prevents a corrupted length byte from being read as a torn tail | lib.rs:884-890; tests/durability.rs:197,210 | tested |
| G5 | Short/unknown header rejected without modification | lib.rs:866-876; tests/durability.rs:238 | tested |
| G6 | After a commit I/O error the handle is poisoned; reopen resolves the outcome from disk | lib.rs:418-424, 586-604; unit test lib.rs:952 covers a write failure only | tested (write failure only; fsync-only, short write and ENOSPC untested) |
| G7 | Second handle/process gets `DatabaseLocked` | lib.rs:376-388; tests/durability.rs:110 (same process only) | tested (same process); cross-process untested |
| G8 | Committed transaction survives process exit without destructors | tests/durability.rs:264; tests/kill_loop.rs (SIGKILL, 8 rounds, unix) | tested (clean exit and SIGKILL; not power loss) |
| G9 | New-database creation syncs file and directories (Unix) | lib.rs:391-397, 934-943 | untested |
| G10 | Only one staged writer; others get `WriterBusy`; drop releases the slot | lib.rs:426-433, 610-617; tests/conditional.rs:392, tests/queue.rs:949 | tested |
| G11 | `compare_exchange` has exactly one winner among 16 threads | advanced.rs:89-128; tests/advanced.rs:~305-340 | tested |
| G12 | Poisoned/panicked mutex surfaces as `Error::Poisoned`, no hang | lib.rs:418-424 | I/O poison tested; no test panics a thread holding the lock |
| G13 | Queue: no double-claim of an unexpired job; stale tokens fenced; ack atomic with business/outbox writes | queue.rs:307-312, 455-536; tests/queue.rs:143,301,624 | tested |
| G14 | Agent checkpoint compare-and-set atomic with extra writes; torn frame never publishes only the business write | agent.rs:267-322; tests/agent.rs:43,575 | tested |
| G15 | Backup/restore detect truncation, trailing bytes, CRC and frame errors; never overwrite | maintenance.rs; tests/maintenance.rs:126-317 | tested |
| G16 | Backup/restore cleanup after a returned error; concurrent restore of in-progress backup gets `DatabaseLocked` | maintenance.rs:77-81, 60, 108 | untested |
| G17 | Search budget overflow errors rather than returning partial results; deterministic ordering | search.rs:238-242, 368-379; tests/search.rs:48 | tested |
| G18 | Changefeed returns whole transactions in order; torn tail yields no gap | advanced.rs:133-178; tests/advanced.rs:372,419,507 | tested (single handle only) |
| G19 | Power-loss durability via `sync_all` | lib.rs:600 | untested; relies on std/OS semantics (Rust std documents `File::sync_all` as using `fcntl(F_FULLFSYNC)` on Apple platforms; the effect on real hardware/filesystems is untested) |
| G20 | Cross-process exclusion against non-cooperating writers | n/a | not provided (advisory lock) |

## 4. Verification of the 12 listed limitations

| ID | Status | What the code does (evidence) | Recommendation |
|---|---|---|---|
| L1 single handle/writer | confirmed limitation (by design) | Non-blocking exclusive `fs2` lock, `DatabaseLocked` (lib.rs:378-389; tests/durability.rs:115); one `Mutex<Inner>` for all operations (lib.rs:179,418). | Document only; sidecar pattern for sharing. Optional bounded-wait open. No multi-process writers. |
| L2 RAM-resident state | confirmed limitation (by design) | Full `Value` trees in memory (lib.rs:156-163); `max_state_bytes` default 67,108,864 enforced at commit (lib.rs:110-119, 734-771); replay applies all frames. | Do not build disk-resident store. Document that RSS exceeds the limit and that lowering limits can block reopen (D1). |
| L3 exact vector search only | confirmed limitation (by design) | Scan of all candidates, per-document parse and normalize, full sort (search.rs:214-300, 398-418); hard error at budget (238-242); no ANN code in src. Note "limited budget" is a fail threshold, not truncation. | Document; benchmark exact path at 1k/10k/100k; optional heap/allocation tweaks. ANN only via RFC. |
| L4 embeddings/workers app-managed | by design | No model, network or thread code in src (grep per auditor). Queue and agent APIs are passive and caller-driven (queue.rs:392-740). | Document-only; optional re-embed recipe and worker examples outside core. |
| L5 session-scoped indexes | confirmed limitation (by design) | `State.indexes` in memory (lib.rs:162); `create_index` writes nothing to the log (query.rs:327-353); cap 64; equality only (query.rs:279-290). | Optional additive `Options::indexes` built after replay; do not persist index data. |
| L6 no shared snapshot across calls/pages | confirmed limitation (by design) | Each call locks and releases; `next_after` is just the last key (query.rs:440). Records inserted behind the cursor are missed. | Document a consistent-read recipe (conditional batch or revision check). Possible later: multi-get under one lock. |
| L7 no MVCC | confirmed limitation (by design) | Single value per document; one writer slot (lib.rs:426-436, 439-466). | Do not build. Snapshot handle only via RFC. |
| L8 no columnar storage | confirmed limitation (by design) | Row-oriented map; `aggregate` is a full scan (lib.rs:467-505). `Database::aggregate` exists at lib.rs:467 (type `Aggregate` at lib.rs:139), a full scan with a finite-f64 sum check (lib.rs:475-500). Resolved by the completeness critic reading the code; an earlier auditor grep was wrong. | Do not build. |
| L9 no WAL/compaction/retention | confirmed limitation | Log is the database; only tail truncation exists (lib.rs:878-929). Cursors, revisions and tokens use `frame_offsets.len()` (agent.rs:189,267; queue.rs:491; conditional.rs:136,144; advanced.rs:140-198). | RFC first (see section 12). Document sizing now. |
| L10 no replication | confirmed limitation (by design) | No follower or network code. Primitives: `changes()`, `backup()`, `restore_backup` (advanced.rs:133-173; maintenance.rs:49-80). No public API applies a changefeed transaction. | Document redundancy recipes; RFC only for a one-way applier. |
| L11 no exactly-once for external effects | by design | Fencing protects engine state only; expired non-final attempts are re-claimed (queue.rs:372, 450-451, 490-499). | Document idempotency-key recipes. |
| L12 not validated for 1.0 | confirmed limitation | README.md:9; roadmap gates (docs/roadmap.md:17-28); no fuzz, fault injection, fixtures or SECURITY.md (auditor listing); CI covers fmt, clippy, tests on 3 OSes, native builds and interop (ci.yml). Roadmap row 0.8 is still "planned" while the 0.8.0 release shipped the CLI. | Implement validation work before 1.0 (P1). |

## 5. Confirmed defects and risks

Severity below is the audit's reconciled rating after verifier votes, not the finder's. None is a demonstrated data-loss bug. None was reproduced by running code.

**D1. Reopening with stricter Options reports intact data as Corruption** (low; unanimous 3/3 confirmed)
- Evidence: lib.rs:898-901 rejects `payload_bytes > max_frame_bytes` as `Corruption`; lib.rs:919-922 maps any `prepare_changes_owned` error (document/state limits) to `Corruption`. Limits are not in the 16-byte header (lib.rs:34). docs/backups.md:22 and docs/jobs.md:64 mention larger limits only for restore/reopen, not as a general rule.
- Root cause: limits are caller policy but replay treats violations as file damage. A genuinely corrupt length is classified the same way, so a distinct variant is partly a design choice.
- Fix: return a distinct limit error from recovery for limit violations (keep `Corruption` for CRC/complement failures); document "limits must not shrink". Perf impact: none. Complexity: low. Compatibility: adds an error variant (public enum change; bindings must map it).

**D2. One-shot `put`/`delete` can return spurious `WriterBusy`** (medium-to-low; 3/3 confirmed, severity "low to medium")
- Evidence: advanced.rs:62-72 calls `begin_write()` (sets `writer_active`, releases mutex; lib.rs:426-436) and `commit()` re-locks later (lib.rs:555). Another thread's mutator in that window gets `WriterBusy` (lib.rs:428-430). `compare_exchange` goes straight through `commit_operations`. No test covers concurrent `put`. No docs mention this for one-shot helpers. Window is short (validation and staging, no fsync). Bindings avoid it (Python per-call mutex, Node synchronous).
- Root cause: one-shot helpers reuse the staged-writer slot.
- Fix: have one-shot helpers take the mutex once and commit directly, without `writer_active` (or wait on a Condvar). Add a multi-thread `put` test; document `WriterBusy`. Perf: neutral or slightly better. Complexity: low. Compatibility: removes a spurious error; callers with retry loops unaffected.

**D3. Queue scans deep-clone every job payload** (low-to-medium; 3/3 confirmed mechanism, severity lowered by all three)
- Evidence: `read_job` clones `document.value` and re-validates the payload for every row in claim, stats and recover (verifiers place it at queue.rs:183-196, not 133-134 as the finder wrote), all under the mutex. Done/dead jobs are never removed; docs/jobs.md documents the O(n) scan and no pruning, but not the per-row clone. The 200 MB figure is an unmeasured estimate.
- Fix: check status/priority/lease fields in place and deserialize only the selected job; later add a prune API or in-memory ready index. Perf: large constant-factor win on large histories. Complexity: medium. Compatibility: none for the in-place read path; a prune API is additive (but see RFC on frame-ordinal semantics).

**D4. Backup is written in place at its final name** (low-to-medium; 3/3 confirmed, two raised "low")
- Evidence: maintenance.rs:58-75 uses `create_new` on the final path; cleanup (77-81) runs only on a returned `Err`. A crash leaves a file with a full-looking header and the final name; a retry fails because `create_new` refuses it. Restore rejects it by length and CRC, so no corrupt data is accepted. docs/backups.md:32-33 warns about partial leftovers in restore context; wording is ambiguous for backup.
- Fix: write to a unique temp name in the same directory, `sync_all`, no-replace rename or hard-link, sync directory. Perf: none. Complexity: low. Compatibility: none (format unchanged).

**D5. A crash during restore leaves a destination that `open` accepts as shorter history** (disputed: low-to-medium; votes 2 confirmed, 1 by-design)
- Evidence: maintenance.rs:121-133 creates the directory and `cedro.log` and copies in place; `recover()` truncates a short tail and returns Ok (lib.rs:866-873, 907-910); a cut exactly on a frame boundary opens silently. docs/backups.md:32 says a crash "may leave a partial destination" but not that it opens successfully. The by-design voter pointed to docs/backups.md:24 (open, verify, check invariants after restore) and that the source backup is untouched. Impact requires a crash plus an operator who opens without retrying. Part of the torn-payload path of `recover()` was not read by one verifier.
- Fix: restore into a sibling temp directory then no-replace rename, or a completion marker that `open` refuses. At minimum, document that a partial destination opens. Perf: none. Complexity: low-medium. Compatibility: a marker file adds a new on-disk artifact; temp-and-rename does not.

**D6. Node binding has no typed or coded errors** (low-to-medium; 3/3 confirmed)
- Evidence: every engine error maps to `GenericFailure` with message text only (verifiers: `engine_error`, closed error near bindings/node/src/lib.rs:198; the finder cited lib.rs:15-17, 56-60). `index.cjs` exports no error classes; tests match `/closed/` on the message. Python exports `CedroError` and `DatabaseClosedError`. The wrapper does throw `TypeError`/`RangeError` for client-side argument checks.
- Fix: export error classes or a stable `code` property mirroring Python. Perf: none. Complexity: low. Compatibility: additive if message text is kept.

**D7. Publish workflow does not validate the supplied `run_id`** (low; 2 confirmed, 1 by-design)
- Evidence: .github/workflows/publish-registries.yml:6-9, 34-45, 61-65 downloads the `distributions` artifact from any run; nothing checks conclusion, branch or SHA. Mitigations: manual `workflow_dispatch` (write access) and protected environments whose reviewer settings were not checked. The distributions artifact needs the test and native jobs to pass, which narrows the "failed run" case; a non-main or release/** run is still possible.
- Fix: `gh run view` check of conclusion, head branch and SHA; publish from tags. Complexity: low. Compatibility: workflow only.

**D8. Test-evidence gaps** (these are gaps, not runtime defects)
- *No real kill/power-loss/fault-injection test* (finder: high; verifiers: confirmed gap but "high" disputed; one voted by-design). tests/durability.rs:263-294 exits cleanly after `commit()` returned. Exhaustive truncation (tests/durability.rs:122-168) covers every prefix a kill could leave, so write-ordering regressions, partial `write(2)`, fsync failure and ENOSPC remain uncovered. README.md:184 and docs/roadmap.md:21 disclose this. Rated **medium** here; it is the single largest validation gap. Fix: kill-loop harness plus a test-only I/O trait with failpoints. Perf: none if cfg(test). Complexity: medium.
- *No fuzz or property tests* (medium; 2 confirmed, 1 by-design as a documented pre-1.0 item: docs/roadmap.md:13,20; docs/releasing.md:18). Hand-picked valid-CRC-bad-frame cases exist (tests/maintenance.rs:236). No demonstrated panic or OOM. Fix: cargo-fuzz on log replay and backup reader; proptest of op sequences versus a model map. Complexity: medium.
- *No historical format fixtures* (medium; 3/3 confirmed). Every test builds logs with current code; docs/migrations.md repeatedly says formats are "unchanged" and nothing checks it. docs/roadmap.md:14,22 plan fixtures for 0.9. Fix: commit logs and backups from each release plus open/verify/restore tests. Complexity: low-medium.
- *CI gaps* (low; 3/3 confirmed): no MSRV (1.74) job although Cargo.toml:5 declares it and README.md:194 states it; debug-only Rust tests (wheel build is `--release`); no audit/deny, no sanitizer or miri; benchmark crate (outside the root workspace) never built; Node 22 and Python 3.11 only.
- *Benchmark doc staleness* (low): docs/benchmarks.md:3 says "0.7" while Cargo.toml:3 is 0.8.0. The "table contradicts reopen comparison" part of the finding was refuted by two verifiers (different workloads); the unpinned baseline commit ("original main") is real.

**Risks (not defects)**
- Power-loss behavior unverified on any filesystem; depends on `File::sync_all` semantics (README.md:184).
- Tests never inject `sync_all`-only failure, short writes or ENOSPC. Both seek and write/sync failure branches set `poisoned` by reading (lib.rs:590-603).
- Panic while holding the mutex (for example `expect` near lib.rs:~715/756; fixed-size slice `unwrap`s in recover, which verifiers judged infallible) permanently poisons the handle; `apply_changes` mutates state change by change (lib.rs:817-868), hidden only because the poisoned mutex stops observation.
- A leaked or parked `WriteTransaction` holds the writer slot indefinitely; other mutators only get `WriterBusy`. No lock timeouts or cancellation exist.
- Commit peak memory is prepared clone plus payload plus frame buffer for up to `max_frame_bytes` (16 MiB default).
- Changefeed, verify and backup share the writer's file descriptor and rely on re-seeking before each append (lib.rs:581); seeks seen at maintenance.rs:34,56,65 and advanced.rs:204, not every path audited.
- Restored or forked histories can reuse cursors, tokens and revisions; changefeed cursors carry no history identity (documented at docs/jobs.md:50, docs/backups.md:26; code does not enforce).
- Caller-supplied `now_ms`: skew between workers can cause concurrent external effects (documented in docs/jobs.md).
- Windows: directory sync is a no-op (documented, docs/architecture.md:57); `remove_file` cleanup with open handles untested.
- Symlinked parent directories of backup/restore paths are followed (normal POSIX; untested).

## 6. Disputed and refuted claims (do not re-report)

Refuted:
- **Nesting limit mismatch Node 64 vs Python 128.** The engine enforces 64 (`MAX_DOCUMENT_DEPTH`, lib.rs:44, 656-673) and the READMEs document it; Python's 128 is a looser client pre-check, so a depth-100 document is rejected by the engine.
- **No FFI panic guard.** Cited `unwrap`s are on fixed-size slices of a 12-byte array (lib.rs:887-895); the `expect` at lib.rs:756 guards an invariant. pyo3 converts panics; Python `close()` recovers a poisoned mutex on purpose (python/src/lib.rs:172-180). Node panic behavior was never verified by anyone. A small residual: Python's `closed` getter raises when poisoned.
- **Poison/I-O only tested by flipping a flag.** False for the main path: lib.rs:952-985 uses a read-only descriptor so `write_all` fails for real, asserting `Error::Io`, poisoned diagnostics, `Poisoned` on later calls and absence after reopen. Residual: no `sync_all`-only, partial-write or ENOSPC test.
- **Cross-process lock untested / weak concurrency tests.** `fs2` uses flock (Unix) / LockFileEx (Windows), per open file description, so the same-process test exercises the real lock; thread tests also exist at tests/agent.rs:152 and tests/queue.rs:156. Residual: no live second-process `DatabaseLocked` test.
- **Vendored core drift as a release risk** (also stale-dist artifacts). `bindings/*/core` and `dist` are gitignored; Node `build`/`prepack` and Python `prepare_core.py` regenerate them; CI builds from a clean checkout. Local leftovers only.

By design (documented or intentional):
- Zero-filled complete tail refuses open (lib.rs:884-890; pinned by tests/durability.rs:197,210; README "without silent repair"). Residual: an opt-in repair mode would be an enhancement.
- No version negotiation on header (single format version; docs/migrations.md:23). Residual: a dedicated `UnsupportedVersion` error would be nicer.
- Partial header (1 to 15 bytes) after a crash at creation. Disputed 1 confirmed, 1 unclear, 1 by-design; header is 16 bytes written by one `write_all` (the finder's "8+" was wrong); tests pin rejection without repair (`partial_or_unrecognized_file_headers_are_rejected_without_repair`). No user data exists at that point. Treated as by design, tiny window.
- Directory sync no-op on non-Unix and only two levels on Unix (docs/architecture.md:57).
- Poisoned handle keeps the lock until dropped (error text says "close and reopen"; test `failed_commit_poisons_handle_until_reopen`).
- Readers block during fsync/backup/verify/changes; clones under lock; query holds mutex for the full scan; `max_state_bytes` not an RSS bound; unbounded `frame_offsets` and log; query page bounded by record count not bytes (bounded in practice by state size); reserved collections readable via generic reads (docs/jobs.md:60); mixed int/float ordering error (src/query.rs:182-199, docs/native-libraries.md:77); changefeed cursor without history identity; BM25 stats over the filtered set (docs/agents.md:104); one malformed vector or job row fails the whole search or queue scan (tested, documented); NFD tokenization and no stemming (docs/agents.md:100-102); no purge of done/dead jobs; no permanent-failure flag or backoff (caller supplies `retry_delay_ms`); no atomic ack plus checkpoint (docs/agents.md:57-58); caller clock (docs/jobs.md); backup holding the mutex over two passes; verify/restore RAM; default umask file permissions (the live log is also created without a mode); Node unsafe-number rejection on read and write, including large floats; Node synchronous API without `Symbol.dispose` (documented); benchmark limits; stale local core copies.
- Search memory/CPU per query "not bounded by limit" (`sv-memory-full-sort-per-query`): 2 verifiers confirmed a minor inefficiency, 1 called it by-design. Bounded by `max_scanned` (at most 100k) and capped terms; per-document `Vec<f64>`s are transient. Only the final full sort is avoidable. Hybrid mode needs all scores and `matched` needs the full candidate count. Low-value optimization; treat as P3.
- A misleading doc comment says "read lock" at src/search.rs:116 while a `Mutex` is held (cosmetic; fix the comment).

## 7. Prioritized improvement matrix

| Pri | Item | Type | Complexity |
|---|---|---|---|
| P0 | Fault-injection and kill-loop durability tests (D8a) | test | medium |
| P0 | Fuzz log replay and backup reader (D8b) | test | medium |
| P0 | Historical format fixtures in CI (D8c) | test | low-medium |
| P1 | Distinct limit error on reopen; document "limits must not shrink" (D1) | defect | low |
| P1 | One-shot `put`/`delete` serialize instead of `WriterBusy` (D2) | defect | low |
| P1 | Atomic backup publish via temp and rename (D4) | defect | low |
| P1 | Restore into temp directory or completion marker; document partial restore opens (D5) | defect | low-medium |
| P1 | SECURITY.md, English format spec, compatibility policy (L12) | docs | low |
| P2 | Queue scan reads in place, avoids payload clone (D3) | perf | medium |
| P2 | Typed Node errors (D6) | api | low |
| P2 | CI: MSRV job, release-mode tests, cargo audit, build benchmark crate (D8d) | ci | low |
| P2 | Publish workflow checks run conclusion, branch and SHA (D7) | ci | low |
| P2 | Compaction/retention design (L9) | RFC | high |
| P3 | Options-based index auto-create (L5) | feature | low |
| P3 | Top-k heap and fewer allocations in search | perf | low-medium |
| P3 | Docs: sizing guidance, RSS, consistent-read recipe, redundancy recipe, idempotency-key recipe, benchmark heading/baseline, fix "read lock" comment | docs | low |
| P3 | Cross-process lock test; multi-thread `put` test | test | low |
| P3 | ANN, MVCC, replication, columnar, multi-process | do not build now | very high |

## 8. Proposed target architecture

Keep the single-process, single-file, fully in-RAM, append-only log with one coarse mutex and fsync-per-commit. Changes are additive and mostly outside the hot path.

- Same file format and frame layout. A future compacted format would add a base cursor (RFC-gated).
- A test-only I/O seam (a thin file trait compiled under `cfg(test)` or monomorphised) so tests can fail or short-write at any byte. No runtime cost expected (not measured).
- Recovery distinguishes limit violations from corruption.
- One-shot helpers commit directly through `commit_operations`; the explicit staged-writer slot remains fail-fast.
- Backup and restore publish atomically (temp then no-replace rename, then directory sync).
- Queue scans inspect stored values in place.
- Optional `Options::indexes` built after replay; indexes stay derived, handle-local and unpersisted.
- Bindings expose stable error codes.
- Compaction, if built, is an explicit offline operation with cursor-continuity rules.

## 9. Implementation roadmap and dependencies

1. **Test foundations first (P0).** The I/O seam enables fault-injection tests. Fuzz targets and fixtures are independent. These should land before any behavior change so regressions are caught.
2. **Recovery error split (D1)** depends on nothing; update bindings (Node codes in D6 should land together or after).
3. **D2 one-shot helpers** depends on the concurrency test being written first (failing test then fix).
4. **D4 and D5 atomic publish** share helper code (temp path, no-replace rename, directory sync). Do D4 first, then D5. Both are tested with the fault-injection seam.
5. **D3 queue read-in-place** is independent; benchmark before and after.
6. **CI work** (MSRV, release tests, audit, publish validation) is independent. MSRV job may expose dependency floor issues.
7. **SECURITY.md, format spec, policy** should precede the 1.0 gate and the compaction RFC (the spec is the baseline for any new format version).
8. **Compaction RFC** depends on the format spec, fixtures and cursor-retention decisions; implementation depends on the RFC and on backup/restore handling of `base_cursor`.

## 10. Acceptance criteria and required regression tests

- **Fault injection:** for every byte offset of a commit frame, an injected short write or EIO yields `Io`, poisons the handle, and reopen recovers exactly the preceding commits and stays writable. Same for `sync_all`-only failure and ENOSPC. A SIGKILL loop against a child writer reopens to a prefix-consistent log.
- **Fuzz:** log replay and backup reader fuzz targets run a short smoke in CI with no panic, OOM or hang; a proptest of random op sequences with reopen equals a model map.
- **Fixtures:** logs and backups from each released version open, verify and restore with exact expected contents; CI fails if fixtures are modified.
- **D1:** reopening with smaller `max_frame_bytes`, `max_document_bytes` and `max_state_bytes` returns the new limit error, leaves the file untouched, and succeeds again with the original limits; real corruption still returns `Corruption`.
- **D2:** N threads calling `put`/`delete`/`compare_exchange` concurrently all succeed with no `WriterBusy`; an explicit transaction still gets `WriterBusy` for others.
- **D3:** claim, stats and recover results are identical to current behavior, including malformed-row `Corruption`; a benchmark shows no regression.
- **D4/D5:** a simulated crash mid-backup leaves no file at the final name; a simulated crash mid-restore leaves no destination openable as a database; success paths unchanged; existing no-overwrite tests still pass.
- **D6:** Node errors expose stable codes for `WriterBusy`, `Poisoned`, `DatabaseLocked`, closed and invalid argument; messages unchanged.
- **D7:** the publish workflow fails for a run whose conclusion, branch or SHA does not match.
- **CI:** a Rust 1.74 job passes; release-mode tests pass; `cargo audit` runs; the benchmark crate builds.
- **Cross-process lock:** a live child process holding the database makes another process's open return `DatabaseLocked`.

## 11. Intentional design decisions that should NOT be changed

- **Strict recovery:** truncate only short tails; error on complete corrupt frames. Prevents silently dropping acknowledged data (pinned by tests/durability.rs:185-260). Any "repair" must be explicit and opt-in.
- **Poison on ambiguous commit outcome** and reopen to resolve; unsound to retry after a failed fsync.
- **Single mutex, no MVCC, fail-fast `WriterBusy` for explicit transactions:** simple, deadlock-free, consistent.
- **Fully RAM-resident exact state, exact search with hard error on budget overflow:** deterministic and replayable.
- **Caller-supplied `now_ms`, caller-owned embeddings, workers and outbox dispatch; no exactly-once claim.**
- **Indexes derived and handle-local, not persisted.**
- **Ordinal cursors with retained history** until an RFC defines retention.
- **Backup as raw verified log copy; restore only to a new destination; no overwrite.**
- **Node rejecting unsafe numbers instead of rounding;** reserved-prefix write protection with diagnostic reads.
- **Benchmarks as a separate crate;** no dependency additions to the core crate for models, network or threads.

## 12. Items needing an RFC before implementation

1. **Log compaction and cursor retention:** base cursor format, `CursorCompacted` error, effect on checkpoint revisions, lease tokens, backups, deletion semantics, format version bump, consumer tracking.
2. **History identity** (epoch/ID in log or cursor) to detect restore-and-diverge forks; prerequisite for any replication.
3. **Persisted index definitions** (log record type, replay, verify, backup, 64-index cap).
4. **Read snapshot handle** (copy-on-write or `Arc` state) with optional page pinning.
5. **One-way changefeed applier / replication** (contiguous cursor checks, promotion, history identity).
6. **Optional in-memory ANN index** (opt-in flag, approximate marker in `SearchPage`, exact search as recall oracle, prefilter handling, hybrid ranking).
7. **Queue pruning / retention API,** because it interacts with reserved id semantics, enqueue idempotency and cursors.
9. **Fault injection for commit I/O** (fsync-only failure, short write, ENOSPC): `Inner.file` is a concrete `File`, so injecting faults needs a file-abstraction seam in `persist_operations`, which touches the release commit path. Not done; `tests/kill_loop.rs` covers only SIGKILL (OS page cache survives, so it says nothing about power loss or fsync semantics).
8. **Opt-in repair mode** for a complete but invalid tail frame, including a policy for zero-filled tails.

## Not verified by this audit

Test pass status on any platform; real power-loss behavior on ext4, APFS, XFS or NTFS; real-hardware effect of `F_FULLFSYNC` on macOS; ENOSPC behavior; napi-rs panic behavior; Windows lock and directory-sync behavior; latency and RSS under load; GitHub environment reviewer settings; behavior of the Node and Python wrappers when `nowMs` is omitted; every test body; the Python CLI.


## Addendum: critic findings (open)

- A crash between `create(true)` and the header write leaves a zero-length `cedro.log`, which `open` treats as a new database (lib.rs:390); benign and distinct from the 1-15 byte partial-header case.
- Not yet analysed: upgrade/downgrade matrix (newer log on older reader), queue/agent crash-window table (enqueue ack lost, claim before start, effect before ack, checkpoint and ack as separate commits), binding API surface versus engine, `max_frame_bytes` up to u32::MAX built in RAM, Windows lock/directory-sync behavior.
- Unbounded log growth (L9) is arguably higher than P2 for long-running agent workloads; D2 (spurious `WriterBusy` on `put`/`delete`) is a cheap user-visible fix and is promoted to the first implementation batch.
- Vendored engine copies in `bindings/node/core` and `bindings/python/core` drift from `src/`; needs a CI drift check.
