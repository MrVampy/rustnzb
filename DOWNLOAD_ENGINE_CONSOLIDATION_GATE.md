# Gate: consolidate rustnzbd download-engine ownership

## Status

- Classification: blocking architecture gate
- Blocks: `DUPE_ARTICLE_FALLBACK_PORT_PLAN.md` and any new dispatch behavior
- Scope: existing engine ownership and parity only; no duplicate-recovery code
- Reviewed repository revision: `32dfb5a`
- Review date: 2026-07-21

## Decision

Make `nzb-dispatch` the sole owner of article dispatch, provider retry,
connection accounting, work scheduling, decode/assembly dispatch, and dispatch
progress events. Make `nzb-web::QueueManager` consume that functionality only
through a stable `DispatchEngine` contract.

Do not switch QueueManager directly to the current `nzb-dispatch`
implementation. The active `nzb-web` engine contains newer production fixes
that are absent from the duplicated `nzb-dispatch::WorkerPool`. First reconcile
and test those behaviors in `nzb-dispatch`; then switch the consumer; finally
delete the duplicate modules from `nzb-web`.

This gate is complete only when one production engine implementation remains.
It is not complete when both copies merely happen to match.

## Why this is a blocking gate

The repository currently has three related engine surfaces:

1. `crates/nzb-web/src/download_engine.rs` — the engine actually constructed
   and called by `QueueManager`.
2. `crates/nzb-dispatch/src/download_engine.rs` — a diverged copy of the same
   `WorkerPool` design.
3. `crates/nzb-dispatch/src/news_engine.rs` — a second
   `DispatchEngine` implementation backed by `nzb-news`.

`nzb-web` declares a dependency on `nzb-dispatch`, but its queue manager does
not use the dispatch trait or either exported dispatch implementation. This
creates several correctness risks:

- fixes can land in a nominal shared crate without changing the application;
- application fixes can leave the reusable dispatcher stale;
- tests may validate a backend that production never constructs;
- public types such as `ProgressUpdate`, `ArticleFailure`, bandwidth policy,
  and connection accounting can acquire incompatible meanings;
- new features would need to be implemented multiple times or would behave
  differently depending on construction path; and
- downstream crates cannot rely on the stated `nzb-dispatch` boundary.

Duplicate article fallback must intercept terminal provider exhaustion and
alter retry accounting. Implementing it before this consolidation would make
the ownership defect materially harder to unwind.

## Current-state findings

### Production construction path

`QueueManager::new` currently constructs these `nzb-web` types directly:

- `BandwidthLimiter`
- `ConnectionTracker`
- `WorkerPool`
- `ProgressUpdate`
- `build_job_submission`

It then calls concrete pool methods for submit, pause, resume, cancel, abort,
server reconciliation, completion release, shutdown, liveness, and eviction
metrics. That means the existing `DispatchEngine` trait is not yet sufficient
for the production consumer.

### Exact duplicate modules

At the reviewed revision:

- `crates/nzb-web/src/article_failure.rs` and
  `crates/nzb-dispatch/src/article_failure.rs` are byte-identical.
- `crates/nzb-web/src/bandwidth.rs` and
  `crates/nzb-dispatch/src/bandwidth.rs` are byte-identical.
- The two `download_engine.rs` files are not identical: the diff is hundreds
  of lines and contains behavioral, API, lifecycle, metrics, and test changes.

The identical files should become re-exports from `nzb-dispatch`; they should
not remain synchronized copies.

### Production behavior missing from the dispatch copy

The `nzb-web` engine currently contains behavior not present in the duplicated
`nzb-dispatch::WorkerPool`, including at least:

- established-socket accounting via `connected_snapshot`;
- active download duration excluding queue wait and pauses;
- yEnc filename propagation used for early obfuscated PAR2 classification;
- explicit release of completed job/assembler handles before post-processing;
- the stronger rule that unavailable/circuit-broken providers do not prove
  global article absence;
- tests for typed provider exhaustion and active duration; and
- newer connection lifecycle tests.

Conversely, the `nzb-dispatch` copy contains work-queue round-robin behavior and
trait-facing helpers that do not exist in the production copy. Consolidation
must reconcile both directions deliberately; copying either file over the
other would lose behavior.

### Trait gaps

The current `DispatchEngine` trait covers core control methods but the
production queue also needs:

- release of a completed job's dispatcher/assembler resources;
- established and allocated connection snapshots;
- connection-slot total;
- progress channel capacity or a dispatcher-created receiver/handle;
- server configuration replacement/reconciliation inputs;
- reliable shutdown and terminal ownership guarantees;
- any server-attempt diagnostic snapshot used by hopeless-abort reporting; and
- a construction path that injects bandwidth policy, connection limits, and
  article timeout without exposing concrete `WorkerPool` internals.

Do not add an escape hatch returning the concrete pool to QueueManager. Every
escape hatch recreates the coupling this gate is intended to remove.

## Target boundary

```text
nzb-web
  QueueManager
    ├── queue/history persistence
    ├── hopeless/PAR2 policy
    ├── post-processing lifecycle
    └── Arc<dyn DispatchEngine>
          │
          ▼
nzb-dispatch
  DispatchEngine contract + one production implementation
    ├── work queue and scheduling
    ├── provider selection/retry
    ├── NNTP connection lifecycle/accounting
    ├── bandwidth enforcement
    ├── yEnc decode → assembler dispatch
    └── typed ProgressEvent stream
          │
          ├── nzb-news / nzb-nntp
          └── nzb-decode
```

Ownership rules:

- `nzb-dispatch` owns dispatch events and dispatch failure types.
- `nzb-web` owns queue/history state and translates dispatch events into
  persisted application state.
- `nzb-core` owns serializable job/file/article models.
- `nzb-nntp` owns protocol/provider errors and server configuration.
- `nzb-decode` owns decoding and file assembly.
- No dependency may point from `nzb-dispatch` back to `nzb-web`.

## Backend decision

There must be one default production backend after this gate. Choose it with a
short architecture decision record based on measured parity, not naming.

Recommended path:

1. Treat the production `nzb-web::WorkerPool` behavior as the compatibility
   baseline because it currently serves rustnzbd users.
2. Move/reconcile that baseline into `nzb-dispatch`.
3. Port any proven `nzb-dispatch` improvements, including fair scheduling,
   behind the same contract and parity tests.
4. Decide whether `NewsDispatchEngine` replaces the reconciled pool or is
   removed. Do not leave it as an unselected second production-capable backend.

If `NewsDispatchEngine` is retained temporarily for migration experiments, it
must be test-only or behind an explicitly non-production feature, with a dated
removal/selection issue. The final gate still requires one production backend.

## Required behavior contract

Freeze these behaviors before moving code. Tests should observe the public
contract rather than private implementation details.

### Submission and terminal ownership

- Each unfinished article resolves exactly once.
- A job emits exactly one terminal event.
- Cancelled/deleted jobs do not later emit completion.
- Aborts drain queued/in-flight accounting without producing zombie jobs.
- Completion releases assembler file descriptors before PAR2/unpack opens the
  files.
- Progress-channel pressure is bounded and cannot silently strand a job.

### Provider failure semantics

- A provider `430` is article-absence evidence for that provider only.
- Connection, timeout, auth, permission, protocol, quota, and circuit-breaker
  states are typed distinctly.
- An unavailable provider does not count as definitive global absence.
- Retry/failover preserves per-provider attempt statistics.
- Local decode/assembly/disk failures are not misreported as provider absence.

### Scheduling and control

- Server priority is honored before backup providers.
- Multiple active jobs receive fair service without defeating priority.
- Pause/resume and global pause preserve their existing semantics.
- Priority preemption and active-download limits remain deterministic.
- Server add/remove/disable/resize during download cannot strand work.
- Worker watchdog eviction cannot discard in-flight ownership.

### Runtime accounting

- Allocated connection slots never exceed configured limits.
- Established connection counts exclude disconnected permit holders.
- Speed limiting and byte accounting match the existing API.
- Active download duration excludes queue wait and pauses.
- yEnc filenames reach the queue layer early enough for obfuscated PAR2
  classification.
- Shutdown closes workers and sockets and persists resumable queue state.

### Resume and post-processing handoff

- Downloaded article checkpoints reconstruct only unfinished work.
- Resume does not double-count completed articles/bytes/files.
- The last dispatch event cannot race a released assembler handle.
- Post-processing begins only after the dispatcher has relinquished mutation
  of the job's files.

## Implementation plan

### Phase A: freeze parity tests

- Introduce integration tests that construct QueueManager through an injected
  `Arc<dyn DispatchEngine>` or a test factory.
- Capture current production behavior for all contract sections above.
- Add a deterministic trace type for test comparison: ordered terminal state,
  per-article outcomes, provider attempts, connection snapshots, active time,
  and final file checksums. Do not assert nondeterministic worker log order.
- Run the same conformance suite against the production baseline and the
  candidate `nzb-dispatch` implementation.

Exit criterion: known differences are listed and classified as a production
regression, intended improvement, or irrelevant implementation detail.

### Phase B: complete the dispatch contract

- Replace the current concrete-pool assumptions with a job submission handle
  or trait methods that cover the full production lifecycle.
- Move `ProgressUpdate` to a contract-focused module and rename it if helpful
  (for example, `DispatchEvent`). Keep queue persistence details out of it.
- Add connection snapshot and completed-job release capabilities without
  exposing `WorkerPool`.
- Move/re-export `ArticleFailure`, `ArticleFailureKind`, `BandwidthConfig`, and
  `BandwidthLimiter` from `nzb-dispatch` only.
- Add a constructor/factory configuration type for servers, bandwidth,
  connection tracking, and timeouts.
- Remove comments that describe a future extraction as though it were still
  pending after the move.

Exit criterion: QueueManager can compile against a fake dispatcher using only
the public contract, and the real dispatcher implements the same contract
without downcasting or concrete escape hatches.

### Phase C: reconcile the implementation

- Start from the active production semantics and port them into
  `nzb-dispatch` with history-preserving commits.
- Reconcile work-queue fairness separately from failure/lifecycle parity so a
  regression can be bisected.
- Port the missing production tests into `nzb-dispatch` before switching the
  consumer.
- Resolve `NewsDispatchEngine`: select it and prove full conformance, or remove
  it from production compilation. Do not keep two independent event/accounting
  implementations.
- Run focused benchmarks for throughput, queue-lock time, provider failover,
  and multi-job fairness. Consolidation must not introduce a material download
  regression.

Exit criterion: the chosen `nzb-dispatch` implementation passes the frozen
production conformance suite and all intended improvements are explicitly
documented.

### Phase D: switch QueueManager

- Inject `Arc<dyn DispatchEngine>` into QueueManager; production startup uses
  the chosen `nzb-dispatch` factory.
- Replace all concrete `worker_pool` and `conn_tracker` fields/calls with
  contract methods or handles.
- Update queue tests to use the injected fake for queue-policy unit tests and
  the real dispatcher for integration tests.
- Verify app, desktop, DAV, SAB, REST status, settings reconfiguration, and
  shutdown paths through the new boundary.

Exit criterion: no production path in `nzb-web` imports its local
`download_engine`, `article_failure`, or `bandwidth` modules.

### Phase E: delete duplicates and enforce the boundary

- Delete:
  - `crates/nzb-web/src/download_engine.rs`
  - `crates/nzb-web/src/article_failure.rs`
  - `crates/nzb-web/src/bandwidth.rs`
- Remove their module declarations and replace public compatibility exports
  with direct `nzb-dispatch` re-exports only where downstream compatibility
  requires them.
- Add a repository policy check that fails if dispatch implementation modules
  are reintroduced under `nzb-web` or if QueueManager imports concrete engine
  internals.
- Update crate READMEs, rustdoc, architecture docs, and dependency descriptions
  so they describe actual ownership.

Exit criterion: repository search finds one definition of each dispatch event,
failure taxonomy, bandwidth limiter, connection tracker, work queue, and
production dispatcher.

## Required tests

At minimum, the gate must pass:

- existing `nzb-web` harness suites: smoke, slots, priority, global pause,
  liveness, hopeless, typed errors, and zombie prevention;
- `nzb-dispatch` unit and integration suites;
- app mock-download and global-pause integration tests;
- restart/resume tests with partially downloaded multi-file jobs;
- provider matrix tests for 430, timeout, auth, permission, connection loss,
  decode error, and assembly error;
- live connection count and configured slot count API tests;
- server reconfiguration during active work;
- post-processing handoff/file-descriptor release;
- multi-job fairness without priority inversion;
- bounded progress-channel stress; and
- graceful shutdown with active and paused jobs.

Before commit, run the repository gates from `CLAUDE.md`:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Also run frontend/E2E coverage that exercises queue status and server
connection views because the contract carries those values to the API.

## Non-goals

- Duplicate NZB discovery or article fallback.
- New retry policies or provider-selection features beyond reconciling already
  intended behavior.
- Stream repair, archive parsing, or post-processing redesign.
- Public API redesign unrelated to representing existing connection/download
  state.
- Performance rewrites without a parity benchmark and separate justification.

## Rollout and rollback

- Land the gate as a dedicated branch/PR before any duplicate-recovery branch.
- Prefer small commits: tests, contract completion, behavior reconciliation,
  consumer switch, deletion, documentation.
- Keep the consumer switch revertable until CI and a real-provider soak pass.
- Do not carry both engines behind a long-lived runtime flag; that preserves
  rather than resolves the defect.
- After the switch, monitor job terminal counts, zombie/duplicate-terminal
  counters, provider outcome distributions, worker evictions, connection
  counts, throughput, and post-processing failures.

## Definition of done / feature-plan unblock condition

The duplicate fallback plan remains blocked until all of these are true:

- `nzb-dispatch` is the documented and actual sole owner of dispatch behavior.
- QueueManager depends on `DispatchEngine`, not `WorkerPool` or a local engine.
- Exactly one production dispatcher implementation is compiled and selected.
- The duplicate modules under `nzb-web` are deleted.
- Production-only fixes listed in this document exist in the canonical engine.
- Conformance, workspace, app integration, frontend, and E2E tests pass.
- Throughput/fairness benchmarks show no unexplained material regression.
- CI passes on the merged gate commit.
- The architecture documentation and code imports agree.

Only after this gate is complete should work begin on duplicate identity,
donor catalogs, fallback source ordering, or recovery accounting.
