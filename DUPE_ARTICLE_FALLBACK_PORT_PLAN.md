# Port plan: NZBGet `DupeArticleFallback` for rustnzbd

## Document status

- Reviewed upstream feature: [nzbgetcom/nzbget#850](https://github.com/nzbgetcom/nzbget/pull/850)
- Related request: [nzbgetcom/nzbget#849](https://github.com/nzbgetcom/nzbget/issues/849)
- Upstream revision reviewed: `4c3d194c36af1f0062bc91e7618ff3c2d77dc006`
- Review date: 2026-07-21
- Target repository revision: rustnzbd `main` at `4f89fc8`
- Scope of this document: design and implementation plan only; no feature code is included

## Executive recommendation

Port the feature, but do not port the pull request as one change and do not
translate its C++ implementation line for line.

The feature solves a real recovery gap: two postings of the same release may
each be incomplete while their union is usable. rustnzbd currently tries a
message-id across configured providers and then records the article as failed;
it does not try an equivalent article from another NZB. The upstream feature
adds a useful recovery ladder:

1. `article`: substitute an equivalent donor message-id during download.
2. `stream`: fill verified missing byte ranges after download, including when
   donor segmentation differs.
3. `live`: run the stream repair early, while other files in the job download.
4. `dupe_stream_decompress`: optionally extract a compressed donor as the
   heaviest stream-repair source.

The appropriate rustnzbd delivery strategy is:

- land duplicate identity, donor indexing, and dispatch ownership first;
- ship a fail-closed `article` implementation next;
- add same-byte `stream` repair only after restart-safe hole tracking exists;
- treat cross-packing, encryption, decompression, and `live` as separate
  milestones, each with its own security and functional-test gate.

This keeps every release useful and independently reversible. It also avoids
coupling a high-value article fallback to approximately 17,000 lines of
archive, crypto, persistence, concurrency, API, and UI work in the current
upstream PR.

### Proposed feature flags and Arr compatibility

Use two independent settings, both disabled by default:

```toml
# Use matching queue/history NZBs as donors for the active target.
dupe_article_fallback = "off" # off | article | stream | live

# When a later matching NZB arrives, allow one bounded retry attempt derived
# from an earlier failed NZB. This changes the external queue contract.
retry_original_nzb = false
```

`dupe_article_fallback` controls recovery of the currently active target. It
never, by itself, reopens a terminal history item. `retry_original_nzb` is a
separate policy because it can create a later attempt after an Arr client has
already received failure and submitted a replacement.

With only donor fallback enabled, the Arr-safe flow is:

```text
NZB A fails and remains a terminal history item
NZB B arrives later with the same duplicate identity
NZB B may use retained A data as a donor
NZB B is the only active target and owns its completion/failure event
```

With `retry_original_nzb=true`, do not silently mutate or resume A. Model the
retry explicitly:

```text
original job A (immutable terminal failure)
  └── retry attempt A#2 (new attempt ID, same client correlation ID)
       └── donor: later matching NZB B
```

The opt-in retry policy must define a maximum retry count (default one), an age
window, eligible failure statuses, cancellation/blacklist behavior, whether B
also downloads independently, output-collision prevention, and Arr-facing API
event semantics. Cancelled, deleted, or manually blacklisted jobs must never be
retried. Until this state machine and its Arr integration tests exist, the
second flag must remain disabled/experimental or return a clear unsupported
configuration error.

## Upstream feature review

### What the pull request currently contains

At the reviewed head, PR #850 is open and mergeable, but GitHub reports an
`unstable` merge state. It contains 88 commits, 65 changed files, approximately
16,945 additions, and 298 deletions. There are no submitted GitHub reviews yet.
All build and functional jobs at the reviewed head pass except the advisory
`cppcheck` job; the contributor reports that its error baseline is pre-existing
and the workflow marks that job non-blocking.

The PR is therefore a strong design and test reference, but not yet a stable
upstream contract to vendor wholesale. Before implementing each later
milestone, re-read the final merged revision or explicitly record which
pre-merge revision rustnzbd follows.

### Capability contract worth preserving

| Mode | Required behavior | Important limit |
|---|---|---|
| `off` | Existing download and PAR2 behavior | Must remain the default |
| `article` | After the primary message-id is definitively exhausted across providers, try equivalent articles from duplicate NZBs without first charging a terminal failure | Requires equivalent file structure and segment mapping; it cannot bridge different packaging |
| `stream` | Includes `article`, then fills missing byte ranges from donors after download | A donor must be proven identical against bytes already downloaded from the target before any patch write |
| `live` | Includes `stream`, with one early repair attempt when a damaged file completes before its job | Post-processing remains the final accounting authority |
| `dupe_stream_decompress` | Opt-in final stream rung that materializes and extracts a compressed donor | Expensive and exposed to untrusted-archive risks; never enable by default |

The configuration should be represented as a capability-ordered Rust enum,
not independent booleans:

```rust
pub enum DupeArticleFallbackMode {
    Off,
    Article,
    Stream,
    Live,
}
```

Accept `no`, `article`, `stream`, and `live`. Accept `yes` as an alias for
`article` for NZBGet configuration compatibility, but always serialize the
canonical value. Do not expose a mode in the UI until that mode's full
acceptance criteria pass.

### Correctness properties to carry into rustnzbd

The most important part of the upstream work is not its archive parsers; it is
the fail-closed behavior added after real-world and adversarial testing:

- A fallback is considered only after all eligible providers definitively
  exhaust the current source. A temporary connection/authentication problem is
  not evidence that the article is missing.
- Changing to a donor source is a retry, not a terminal failure. Job, health,
  and hopeless-download counters are unchanged until every source is exhausted.
- Candidate sources for an in-flight article are pinned. Queue/history churn or
  lead rotation must not reorder the article's remaining source list.
- File matching fails closed when more than one structural candidate is
  plausible.
- Donor yEnc data is decoded and its declared file size and byte placement are
  validated before it is committed to the target.
- Local disk, seek, flush, rename, cancellation, and quota failures never cause
  a switch to another donor; those are not source-availability failures.
- A completed file must tile its declared decoded byte range without gaps or
  overlaps. A provisional donor write must never turn malformed geometry into
  false success.
- Stream repair writes only into captured holes and only after empirical byte
  probes establish donor identity.
- A partially stream-repaired file receives no full-file health credit.
- Recovery state and counters survive restart; cancellation and shutdown fail
  closed instead of accepting partial verification or extraction.
- Donor fetch lookahead is bounded by worker, result-count, and decoded-byte
  budgets, and asynchronous requests own all data they access.

### Upstream risks and deviations to resolve deliberately

1. **Size and maturity.** The upstream PR is still open, has no formal reviews,
   and grew from an article retry into archive mapping, cryptography,
   extraction, and live concurrency. rustnzbd should use separate changes and
   release gates.
2. **Empirical constants.** File-size tolerances, cutover/demotion thresholds,
   identity-probe floors, fetch windows, and decompression caps are policy, not
   protocol facts. Put them in named internal policy structs and cover their
   boundaries with tests before considering public configuration.
3. **Archive extraction.** The PR discussion records a remaining hardening
   concern around validating archive entries and expansion limits before and
   during extractor execution. The rustnzbd decompression rung must not ship
   until bounded preflight, selected-member extraction, link rejection, file
   count, byte, extent, time, and filesystem-containment limits exist.
4. **Licensing.** NZBGet is GPL-2.0 and rustnzbd is MIT. Reimplement the behavior
   from protocol/file-format documentation and black-box tests. Do not copy
   GPL implementation code into rustnzbd without an explicit licensing
   decision. Record specifications and test vectors used for each container or
   crypto implementation.
5. **Product semantics.** NZBGet has mature duplicate keys, scores, backup
   modes, parking, and `ParScan=dupe`. rustnzbd does not currently have an
   equivalent duplicate model or cross-NZB PAR2 sourcing. Those prerequisites
   must be designed rather than assumed.

## rustnzbd architecture assessment

### Existing pieces that reduce the port cost

| Existing capability | Current location | How it helps |
|---|---|---|
| Raw NZB blobs retained for active and history jobs | `crates/nzb-core/src/db.rs`, `crates/nzb-web/src/queue_manager.rs` | Queue and history donors can be reparsed without depending on an external NZB directory |
| Parsed article message-id, segment number, and declared encoded size | `crates/nzb-nntp/src/config.rs`, `crates/nzb-core/src/nzb_parser.rs` | Provides the structural input for article donor matching |
| Typed provider failures and all-provider routing | `crates/nzb-web/src/article_failure.rs`, `crates/nzb-web/src/download_engine.rs` | Provides the correct interception point before a failure becomes terminal |
| yEnc result exposes file size and part offsets | `yenc-simd` through `crates/nzb-decode/src/yenc.rs` | Supports pre-write donor geometry validation |
| In-memory decode before positioned write | `crates/nzb-web/src/download_engine.rs`, `crates/nzb-decode/src/assembler.rs` | rustnzbd can validate donor bytes before `pwrite`, avoiding NZBGet's DirectWrite staging shape |
| Sparse, concurrent positioned assembler and segment bitmap | `crates/nzb-decode/src/assembler.rs` | Provides the base for explicit missing-range tracking |
| Queue checkpoint and SQLite migrations | `crates/nzb-web/src/queue_manager.rs`, `crates/nzb-core/src/db.rs` | Provides a restart-safe persistence path without NZBGet's one-way flat-file format bump |
| Native PAR2 post-processing | `crates/nzb-postproc/src/pipeline.rs` | Remains the final integrity/repair authority |
| Direct unpack lifecycle | `crates/nzb-web/src/direct_unpack.rs` | Gives a reference for later live-repair lifecycle coordination |
| Deterministic mock NNTP facilities | `crates/mock-nntp-server`, `nzb-nntp` test support, app integration tests | Can drive offline complementary-hole and negative scenarios in CI |
| Queue/history detail UI and settings UI | `apps/rustnzb/frontend/src/app/features` | Provides established places for configuration and recovery observability |

Unlike NZBGet, rustnzbd already stores the raw NZB bytes in SQLite for queue
and history entries. The port should use those blobs directly and must not add
an `NzbCleanupDisk=no`-style operational requirement.

### Missing prerequisites and architectural hazards

1. **No duplicate domain model.** `NzbJob` and `HistoryEntry` do not carry a
   duplicate key, duplicate score, duplicate mode, or stable posting
   fingerprint. Same-title lookup alone is not sufficient for reliable donor
   selection.
2. **No donor index.** Queue and history have no query that returns compatible
   donor metadata plus retained NZB data in score order.
3. **Two download-engine implementations have diverged.** The active
   `QueueManager` imports `crate::download_engine` from `nzb-web`, while
   `nzb-dispatch` contains another `download_engine.rs` and a `DispatchEngine`
   boundary. They are not identical. Adding fallback to both would compound
   drift; adding it to only one would make the nominal shared dispatcher lie
   about behavior.
4. **Work items have one source.** `WorkItem` stores a single `message_id` and
   provider outcomes, but no primary identity, pinned donor list, source round,
   reactive/proactive marker, or donor job identity.
5. **No shared per-file fallback state.** Cutover, lead health, expected decoded
   geometry, and recovered counts need state shared by all work items of a file.
6. **Terminal accounting happens immediately.** When
   `handle_article_not_available` determines all providers have definitive
   outcomes, it emits `ArticleFailed`, increments `articles_failed`, and
   resolves the work item. Donor selection must occur before these actions.
7. **Decode geometry is not persisted completely.** `Article` can store
   `data_begin` and `data_size`, but progress handling currently records only
   decoded size. The checkpoint persists downloaded segment numbers, not exact
   ranges, target decoded size, provisional donor state, or holes.
8. **Assembler completion means segment-count completion.** It does not prove
   that written ranges tile the declared file with no gaps/overlaps.
9. **Post-processing has no news access.** `nzb-postproc::run_pipeline` receives
   a directory and config only. Stream repair needs a donor catalog and bounded
   NNTP fetch service and therefore belongs in queue/orchestration immediately
   before the pure post-processing pipeline, with geometry helpers kept in a
   lower-level crate.
10. **Hopeless detection can pre-empt recovery.** Current early and ongoing
    abort checks charge article failures as they arrive. Article fallback must
    run before that charge; stream mode also needs a policy that allows damaged
    jobs to reach the stream stage when recoverable donors are available.
11. **Current archive APIs are extraction-oriented.** There is no random-access
    `ContentSource`, store/copy-mode content map, RAR encryption geometry, or
    selected-member bounded extraction facility.
12. **Recovery metrics do not exist.** Queue/history models, DB rows, API types,
    SAB compatibility fields, Angular models, and detail views need explicit
    recovered and unresolved damage data.
13. **Late donor arrival must not implicitly resurrect terminal jobs.** A later
    replacement job may use an earlier failed job as a donor, but reopening the
    original requires the separate `retry_original_nzb` state machine.
14. **Original-NZB retry needs attempt identity.** The opt-in flag requires new
    attempt IDs, client correlation, cancellation/blacklist handling, bounded
    retries, output de-duplication, and explicit API/history representation. It
    cannot be implemented by calling `resume_job` on a history item.

## Target architecture

Keep the responsibilities separated as follows:

```text
QueueManager / recovery orchestrator
  ├── DuplicateCatalog (queue + history metadata and retained NZB blobs)
  ├── ArticleFallbackCoordinator (matching, pinned source order, cutover)
  ├── DispatchEngine (normal and donor article fetches)
  ├── FileAssembler (validated positioned commits + range ledger)
  ├── StreamRepairController (job-scoped orchestration and accounting)
  │     ├── nzb-repair geometry/content-map helpers
  │     └── bounded donor fetch service
  └── nzb-postproc pipeline (PAR2 → extract → cleanup)
```

The `nzb-repair` name above is conceptual. It may begin as modules in
`nzb-dispatch` and `nzb-postproc`; create a new crate only when doing so removes
a real dependency cycle. Keep NNTP connections and queue/history access out of
`nzb-postproc` so the existing pure directory pipeline remains testable.

### Proposed core data

Add durable duplicate/recovery metadata with backward-compatible serde
defaults and a forward SQLite migration:

```rust
pub struct DuplicateIdentity {
    pub normalized_title: String,
    pub dupe_key: Option<String>,
    pub score: i32,
    pub posting_fingerprint: [u8; 32],
}

pub struct RecoveryStats {
    pub recovered_articles: u64,
    pub recovered_bytes: u64,
    pub recovered_holes: u64,
    pub unresolved_articles: u64,
    pub live_repairing: bool, // runtime/API only; do not persist as active after restart
}

pub struct FileRepairState {
    pub decoded_file_size: Option<u64>,
    pub committed_ranges: Vec<ByteRange>,
    pub holes: Vec<ByteRange>,
    pub failed_segments: Vec<u32>,
    pub staged_donor_segments: Vec<StagedSegment>,
    pub live_attempted: bool,
}
```

`posting_fingerprint` should be derived from canonical file/article structure
and message-ids, not raw XML bytes, so harmless metadata/serialization changes
do not defeat exact-posting detection. A separate structural fingerprint may
be useful for candidate filtering, but it must not be treated as content proof.

Do not silently infer duplicate keys from arbitrary metadata. Parse known NZB
metadata and compatible SAB/NZBGet API parameters deliberately, retain a
normalized-title fallback, and expose the selected identity in debug logs.

## Implementation milestones

### Milestone 0: freeze the behavior and choose dispatch ownership

**Goal:** eliminate ambiguity before feature code changes the download path.

Tasks:

- Re-check PR #850 at its final/selected revision and capture a small behavior
  matrix in tests: modes, source order, structural gates, accounting, restart,
  and negative cases.
- Decide that `nzb-dispatch` is the authoritative implementation, then migrate
  `nzb-web::QueueManager` to its `DispatchEngine`; alternatively document and
  complete removal of `nzb-dispatch`. Do not keep two fallback implementations.
- Extend the dispatch trait only with queue-independent concepts: job
  submission, source retry, bounded standalone fetch, pause/cancel, and typed
  progress. Donor discovery remains above the dispatch layer.
- Add contract tests that run the chosen dispatcher through the queue manager,
  including pause/resume, restart, typed provider errors, and terminal event
  ownership.
- Document the clean-room implementation rule for GPL-derived behavior.

Likely files:

- `crates/nzb-dispatch/src/dispatch_engine.rs`
- `crates/nzb-dispatch/src/download_engine.rs`
- `crates/nzb-web/src/download_engine.rs`
- `crates/nzb-web/src/queue_manager.rs`
- `crates/nzb-web/Cargo.toml`

Exit criteria:

- Only one production article-dispatch implementation is compiled and used.
- Existing queue, retry, pause, hopeless, and resume suites pass through that
  implementation.
- No duplicate-fallback behavior is enabled yet.

### Milestone 1: duplicate identity and donor catalog

**Goal:** make duplicate discovery explicit, cheap, stable, and testable.

Tasks:

- Add `DuplicateIdentity` fields to active/history models and SQLite, using
  serde defaults and the next schema migration after the current v8 schema.
- Parse supported duplicate keys and scores at every intake path: REST upload,
  URL add, SAB API, RSS, watch directory, header-generated NZB, history retry,
  and DAV handoff. Preserve them when retrying or moving to history.
- Define normalized-title matching (Unicode normalization, case folding,
  extension trimming, and whitespace policy) in one function with fixture
  tests. Prefer equal non-empty duplicate keys; use normalized title only when
  neither side has a usable key.
- Compute a canonical posting fingerprint from ordered file identities,
  segment numbers, declared sizes, and normalized message-ids. Skip exact
  postings because they offer no new article sources.
- Add DB/catalog queries that return queue and history candidates ordered by
  score and stable insertion/completion order. Include retained NZB bytes and
  password only when the caller is authorized to use them internally.
- Treat donor lookup as read-only from the active target. With
  `retry_original_nzb=false`, a failed history row may donate but must never be
  enqueued, resumed, or mutated because a matching job arrived.
- Parse donor NZBs outside the queue/jobs and DB locks. Cache immutable
  `Arc<ParsedDonor>` entries in a bounded cache keyed by job ID plus NZB hash.
  Never retain borrowed pointers/references across eviction.
- Invalidate catalog/cache entries on add, rename, duplicate-key update,
  history deletion/pruning, retry, and queue removal.
- Add a read-only diagnostic endpoint or debug log that explains why a donor
  matched or was rejected; do not expose NZB passwords or message-id lists.
- Add `retry_original_nzb` as an independent persisted setting that defaults to
  false. Do not perform retry actions until Milestone 3a is implemented.

Likely files:

- `crates/nzb-core/src/models.rs`
- `crates/nzb-core/src/nzb_parser.rs`
- `crates/nzb-core/src/db.rs`
- `crates/nzb-web/src/queue_manager.rs`
- `crates/nzb-web/src/sabnzbd_compat.rs`
- `crates/nzb-web/src/rss_monitor.rs`
- `apps/rustnzb/src/handlers.rs`

Exit criteria:

- Queue and history duplicates are found consistently by key/title.
- Exact postings and ambiguous identities are rejected.
- History pruning and concurrent queue changes cannot yield stale references.
- Candidate parsing does not hold the queue or DB lock during XML parsing.

### Milestone 2: reactive article fallback (`article` MVP)

**Goal:** recover individual missing articles safely, with no cutover
optimization yet.

Tasks:

- Add `dupe_article_fallback = "off" | "article"` to `GeneralConfig`, its
  example TOML, config API, and settings UI, plus the independent boolean
  `retry_original_nzb = false`. Parse future enum values but reject activation
  with a clear unsupported-mode error until their milestone lands.
- Introduce an `ArticleSource` value containing message-id, primary/donor job
  ID, source kind, and expected donor file/segment identity.
- Extend each work item with its immutable primary source, a pinned fallback
  source vector, current round, and a flag recording whether primary exhaustion
  actually occurred. Pin once at the first fallback; do not rebuild on later
  rounds.
- Intercept only the all-provider definitive branch in
  `handle_article_not_available`. Ask `ArticleFallbackCoordinator` for the next
  source before emitting `ArticleFailed`, incrementing failures, updating the
  hopeless tracker, or resolving the work item.
- Reset provider outcomes/attempt counters for the new message-id while
  preserving provider attempt statistics. Do not reset job cancellation,
  priority, or source history.
- Restrict fallback-triggering errors to remote source failures and donor
  geometry rejection. Local assembly/I/O errors, cancellation, disk guards,
  quota, auth-wide outages, and shutdown must bypass fallback.
- Match donor files fail closed using the upstream gates as the initial policy:
  equal segment count; total declared encoded size within 1/64; corresponding
  part size within 1/16; case-insensitive filename preference; otherwise
  exactly one structural match. Put these constants in `ArticleMatchPolicy`.
- Treat `segment_number` as a lookup key, not a vector index. Reject duplicate,
  missing, zero, or non-contiguous segment-number mappings.
- Before `FileAssembler::assemble_article`, validate donor `file_size`,
  `part_begin`, `part_end`, decoded length, and part number against known target
  geometry. Keep unproven donor data in the existing bounded article cache or
  a recovery staging file; never mark the segment downloaded until validation
  and commit both succeed.
- Persist target decoded size, committed decoded ranges, and any staged donor
  marker needed to revalidate after restart. On completion require exact tiling
  of `[0, decoded_file_size)` with no gap, overlap, underflow, or overflow.
- If exact expected placement cannot be established safely, reject/stage the
  donor and leave the segment for later sources or PAR2. Do not use encoded NZB
  size as decoded placement proof.
- Emit one per-file recovery summary rather than one info log per article.
- Add `recovered_articles` to active and history records. Increment only when a
  source that reacted to proven primary exhaustion succeeds; never count normal
  provider failover or later proactive traffic.

Likely files:

- `crates/nzb-core/src/config.rs`
- `crates/nzb-core/src/models.rs`
- `crates/nzb-dispatch/src/download_engine.rs`
- `crates/nzb-decode/src/assembler.rs`
- `crates/nzb-web/src/queue_manager.rs`
- `apps/rustnzb/config.example.toml`
- `apps/rustnzb/frontend/src/app/features/settings/`

Exit criteria:

- Two structurally equivalent NZBs with complementary holes produce a
  byte-identical output and zero unresolved failures.
- In an Arr-shaped integration test, A emits a terminal failure, then B arrives
  and may borrow retained A data while A remains terminal and emits no retry or
  later completion event when `retry_original_nzb=false`.
- Without the donor, the same primary follows the existing failure/PAR2 path.
- Wrong file size, shifted part boundary, ambiguous file, wrong part number,
  local I/O failure, provider outage, deletion, and restart cases fail closed.
- A fallback round does not alter terminal failure or hopeless counters until
  every pinned source is exhausted.
- `off` is behaviorally identical to the pre-feature baseline.

Enabling only `dupe_article_fallback` must never reopen a terminal history job.
Before Milestone 3a exists, setting `retry_original_nzb=true` must be rejected
or be an explicitly documented no-op rather than silently approximated.

### Milestone 3: article cutover and lead rotation

**Goal:** remove repeated full-provider sweeps when a primary or lead donor is
known to be heavily holed.

Tasks:

- Add one `Arc<FileFallbackState>` per file to the dispatch job context. Track
  reactive recoveries, current lead donor ID, consecutive verified lead misses,
  switch budget, and cutover state.
- After three reactive recoveries by default, mark the file cut over. Fresh,
  not-yet-dispatched articles use `[lead donor, remaining donors, primary]`.
  Already pinned/in-flight articles keep their original vector.
- After three consecutive misses by the active lead, rotate to the next donor
  by stable donor job ID, not candidate-list index. A stale result from an old
  lead must not demote the current lead.
- Bound switches to available donors and re-arm only after a verified lead
  success. If a lead donor disappears, vacate it when sources are pinned.
- Never count any proactive donor success as `recovered_articles`, even after
  it falls through multiple donor rounds. The metric means a hole proven on
  the primary, not traffic served by a preferred source.
- Log cutover and lead switches once per transition with job/file/donor IDs and
  no password/message-id data.

Exit criteria:

- Functional tests prove cutover avoids repeated primary sweeps.
- A bad lead rotates exactly within budget and all donors plus primary remain
  available to every article.
- Candidate insertion/removal and stale in-flight results cannot skip, repeat,
  or misattribute a source.
- Recovered counts remain bounded by primary holes under timing variation.

### Milestone 3a: opt-in retry of original NZBs

**Goal:** when a later matching NZB arrives, optionally create one bounded
retry attempt derived from an earlier failed original without rewriting the
original terminal event.

This milestone is separate from donor fallback and remains disabled by
default. It should not be started solely because the article layer works.

Tasks:

- Add a `RetryAttempt` model with original job ID, new attempt ID, duplicate
  identity, client correlation ID, donor job ID, creation time, retry count,
  and cancellation/blacklist state.
- Add an eligibility query for recent failed/incomplete history rows. Exclude
  cancelled, deleted, manually blacklisted, already retried, and expired rows.
- Create a new A#2 attempt instead of changing A's history row or calling
  `resume_job` on it. A remains an immutable failed history event.
- Pin the later NZB's donor data before it can be removed. Define whether B is
  donor-only or also an active independent target; do not accidentally run two
  copies with the same output destination.
- Define output collision behavior, final history presentation, API event
  ordering, and Arr correlation before enabling the flag.
- Handle cancellation before matching, after queueing, during donor fetch, and
  after partial output. Cancellation must suppress a late completion event.
- Default to one retry within a bounded age window and emit telemetry for
  accepted, suppressed, cancelled, completed, and failed retry attempts.

Exit criteria:

- With the flag off, failed A remains terminal when B arrives.
- With the flag on, A stays immutable and A#2 is a distinct bounded attempt
  with deterministic API/history semantics.
- Arr integration tests cover failure notification, replacement submission,
  blacklist/cancel, successful and failed retry, output collision, and restart
  between matching and retry.

### Milestone 4: restart-safe hole ledger and standalone donor fetch

**Goal:** build the shared infrastructure required by all stream modes.

Tasks:

- Promote the assembler bitmap into a persisted decoded-range ledger. Record
  exact successful ranges and decoded target size; compute holes from metadata,
  never by scanning for zero bytes in a sparse file.
- Persist `FileRepairState` with versioned JSON or normalized tables. Migrations
  must be additive and rollback/documentation tested; old binaries should not
  corrupt new rows even if they ignore unknown columns.
- Add an assembler patch API that accepts only an existing captured hole,
  clips writes to that hole, rejects overlap with committed data, performs
  complete positioned writes, and reports flush/sync failures.
- Extract a bounded `ArticleFetchService` from the authoritative dispatcher.
  It must fetch/decode a donor article without changing normal job counters,
  own its groups/source data, honor cancellation, account provider traffic,
  and support both inline and ordered batch modes.
- For post-processing, use an ordered batch window initially matching the
  upstream bounds (4 workers, at most 8 undelivered results, at most 128 MiB
  decoded). Make these named internal limits and test enforcement, stop, and
  cancellation at boundaries.
- Add a `DupeRepairing` job stage/status, but keep normal post-processing as the
  sole authority that converts repair results into final success/failure.

Exit criteria:

- Exact holes survive restart at every boundary between download and
  post-processing.
- Patch APIs cannot write outside or across a recorded hole.
- Ordered batch delivery stays deterministic under out-of-order fetch
  completion, worker failure, cancellation, and stop.
- Memory/result windows never exceed configured test bounds.

### Milestone 5: same-byte stream repair (`stream` M1)

**Goal:** repair the same posted file from a differently segmented duplicate
without parsing archive internals.

Tasks:

- Add `StreamRepairController` in queue/orchestration immediately after the
  download context closes its assembler handles and before
  `nzb_postproc::run_pipeline`.
- Build target repair jobs from the persisted range ledger. Pair donor members
  by exact filename, then unique volume suffix, then a unique structural/rank
  match. Ambiguity rejects the donor.
- Fetch donor articles covering probe windows adjacent to holes. Compare donor
  bytes with target committed bytes before any patch. Start with the upstream
  16 KiB desired overlap, adaptive reduction for small/holey files, and an
  absolute 64-byte proof floor; expose these as tested policy constants.
- Measure and validate any segmentation drift from multiple probes. Reject
  inconsistent drift, insufficient reachable proof bytes, size mismatch,
  wrong bytes, overflow, or a donor whose declared ranges do not tile.
- Only after the whole target/donor member pairing is verified may donor bytes
  be clipped and written into captured holes. Re-check identity/geometry for
  each fetched patch result.
- Track `recovered_bytes` and `recovered_holes`. Map repaired bytes back to
  failed segments only when an entire failed segment's expected range is now
  covered.
- Separate historical failures from unresolved damage. Keep
  `articles_failed` as what failed on the primary path; derive
  `unresolved_articles` for hopeless/post-processing decisions. Pass zero
  unresolved damage to the later pipeline only when every hole in the affected
  file is filled and synced.
- If any hole remains, grant no full-file health credit; PAR2 receives the
  residual damage normally.

Exit criteria:

- Differently segmented, byte-identical reposts repair to byte-identical output.
- Same-size wrong content, insufficient probes, ambiguous pairing, stop, short
  reads, restart, and disk errors never write unverified bytes or report false
  success.
- A fully repaired job without PAR2 can complete; a partially repaired job
  cannot.
- Existing PAR2 verification remains the final authority when PAR2 data exists.

### Milestone 6: store/copy cross-packing (`stream` M2)

**Goal:** map the same inner bytes across bare files, raw splits, and
uncompressed containers.

Tasks:

- Define a read-only `ContentSource` trait with bounded random reads over both
  target disk files and demand-fetched donor articles.
- Define `ContentMap` as validated inner-content extents mapped onto physical
  member extents. All offset/size arithmetic must use checked operations and
  explicit total-size containment checks.
- Implement one mapper per format, in this order: bare file, raw `.001` splits,
  stored ZIP/ZIP64/spanned ZIP, 7-Zip copy mode and `.7z.001`, then RAR3/RAR5
  store mode.
- Use format specifications and independently generated fixtures. Do not copy
  NZBGet parser code. Reject compression methods, lying counts/sizes, loops,
  overlapping extents, out-of-bounds extra fields, missing headers, and
  unsupported solid/multi-stream layouts.
- Cache a donor content map once per donor set; keep identity probes per
  target/donor pair.
- Leave framing/header holes to PAR2. A damaged member header may disable that
  member without making intact members unsafe, except where a continuous crypto
  stream later requires the full set.

Exit criteria:

- Cross-format positive fixtures repair byte-identically for every supported
  pairing.
- Compressed/unsupported containers never enter the copy path.
- Fuzzing and adversarial fixtures cover parser progress, overflow, truncation,
  forged methods, and containment.

### Milestone 7: password-assisted store-mode encryption (`stream` M3)

**Goal:** bridge supported password-protected store-mode RAR donors/targets.

Tasks:

- Write a separate crypto design note before implementation: RAR3/RAR5 KDF,
  password checks, AES-CBC stream geometry across volumes, random access,
  padding residuals, and secret lifetime.
- Use audited Rust crypto primitives rather than implementing AES. Zeroize
  derived key material where practical; never log passwords, keys, IVs, or
  decrypted probe content.
- Verify donor plaintext by re-encrypting into the target's ciphertext context
  and comparing target ciphertext before patching. Writes to an encrypted
  target must be byte-identical to the target posting.
- Reject wrong passwords, unreadable volumes, incomplete crypto geometry,
  partial trailing blocks that cannot be reproduced, and all compressed RAR
  members.
- Anchor tests to archives generated by independent current WinRAR/7-Zip
  versions and retained ciphertext/check vectors. Run cross-platform builds.

Exit criteria:

- Plain→encrypted, encrypted→plain, equal-password, different-password, wrong-
  password, and non-block-aligned negative fixtures all behave fail closed.
- No secret appears in logs, API output, panic messages, temp paths, or test
  snapshots.
- A security-focused review signs off before the mode is enabled in release
  builds.

### Milestone 8: decompression-assisted donors (`dupe_stream_decompress`, M4)

**Goal:** use an extracted inner file from a compressed duplicate as a final,
opt-in donor.

Tasks:

- Keep this separate from ordinary post-processing extraction. Create an
  exclusive per-attempt temp directory through a race-safe API and an RAII
  cleanup guard that never follows links.
- Probe extractor availability before fetching the donor.
- Preflight archive manifests where the format/tool permits. Reject absolute
  paths, parent traversal, symlinks, hardlinks, devices, FIFOs, alternate data
  streams, duplicate/conflicting paths, excessive nesting, too many entries,
  and ambiguous selected members.
- Materialize only required archive members where possible. Bound declared and
  actual fetched bytes, sparse extents, extracted bytes, selected-member bytes,
  file count, wall time, CPU/process lifetime, and free-disk reserve. Enforce
  limits during output, not just after the extractor exits.
- Run the extractor without a shell, with explicit argv, null stdin, minimal
  environment, contained working/output directories, cancellation, and killed
  process-tree cleanup. Consider a sandbox/container boundary where supported.
- Walk output with no-follow metadata and reject the entire attempt if any link
  or special file exists. Select an inner file only by a unique expected size
  and content proof.
- Feed extracted plaintext through the same M2/M3 verify-then-patch path. Never
  relax identity probes because extraction succeeded.
- Set a conservative default cap and keep the option off by default. Surface
  expected temporary-space cost and skip reasons.

Exit criteria:

- Positive ZIP/7z/RAR compressed donors repair supported plaintext and, after
  M3, store-encrypted targets.
- Archive bombs, many-file archives, sparse extent attacks, link escapes,
  traversal, wrong passwords, missing tools, cancellation, timeout, and
  truncated donors stay inside all limits and leave no temp tree.
- Security review and sanitizer runs pass before UI exposure.

### Milestone 9: live repair (`live`)

**Goal:** overlap one early M1-M3 repair attempt with the remaining download.

Tasks:

- Add a job-scoped live-repair handle with cancellation token, one-at-a-time
  scheduling, and explicit ownership in `JobState`.
- Dispatch only when a damaged file reaches terminal article state while other
  job files remain. Skip the last file because normal post-processing is about
  to start.
- Keep donor fetching inline/serial initially so live repair does not consume
  additional pool connections beyond the active download. Keep decompression
  post-processing-only.
- Coordinate with direct unpack and filename deobfuscation. A file cannot be
  read, renamed, patched, or extracted concurrently without a per-job file
  lifecycle agreement. Re-resolve current paths immediately before every open.
- Live repair may shrink persisted holes but must not credit health, drain the
  repair job, or make the final success decision. The normal stream stage
  reopens state, verifies what remains, and performs all accounting.
- Stop and await live work on job deletion, pause requiring teardown, retry,
  history transition, application shutdown, and post-processing selection.
  Dropped handles or cancelled tasks must not leave a `live_repairing` flag set.

Exit criteria:

- A throttled functional scenario proves repair writes occur before the job's
  download completes.
- Plain `stream` never runs early and the last file never starts a redundant
  live pass.
- Direct unpack, rename, delete, restart, cancellation, and shutdown stress
  tests show no concurrent file mutation, deadlock, dangling task, or double
  accounting.

### Milestone 10: API, UI, compatibility, and rollout completion

Observability should be added alongside the milestone that produces each
value; this final milestone verifies consistency across every surface.

Tasks:

- Persist and expose `recovered_articles`, `recovered_bytes`,
  `recovered_holes`, and `unresolved_articles` on active jobs, history, download
  statistics, REST/OpenAPI, and the applicable SAB compatibility response.
- Expose runtime `live_repairing` and `DupeRepairing` status. The flag must be
  false after restart until a new task is actually attached.
- Add Angular queue/history detail rows and a live Repair badge. Format bytes
  with JavaScript-safe number handling; if values may exceed the exact integer
  range, serialize as strings or split/join with `BigInt` rather than relying
  on 32-bit shifts.
- Add settings help that states capability ordering, matching rules, retained
  NZB behavior, failure limits, resource cost, and the default-off posture.
- Document interaction with hopeless abort, required completion, PAR2,
  post-processing level 0, direct unpack, deletion, history retention, server
  reconfiguration, DAV, archive passwords, Arr terminal events,
  `dupe_article_fallback`, and `retry_original_nzb`.
- Add OTEL counters/histograms for donor matches/rejections, fallback attempts,
  recovered units, verification failures, repair duration, fetched donor bytes,
  cache behavior, cap hits, and cancellations. Keep identifiers low-cardinality.
- Add release notes and database backup/rollback guidance.

Exit criteria:

- REST, OpenAPI, SAB, Angular types, DB restore, and history retention agree on
  field semantics.
- UI tests cover zero, article-only, byte repair over 4 GiB, partial repair,
  live state, and history rendering.
- Default/off users see no changed download behavior or meaningful resource
  regression.

## Test strategy

### Unit and property tests

- Duplicate normalization, key precedence, fingerprints, score ordering, exact
  posting exclusion, cache invalidation, eviction, and ambiguity.
- File match tolerance boundaries and malformed segment numbering.
- Pinned source ordering, reactive/proactive truth, cutover, lead rotation,
  stale results, deletion, and switch budgets.
- Decoded file-size/part geometry, checked arithmetic, tiling, staging,
  demotion, patch containment, and hole merging/splitting.
- Ordered fetch delivery and hard bounds under randomized completion orders.
- Content-map parsers with property/fuzz tests that require progress or a
  bounded error for arbitrary bytes.
- Crypto vectors and random-access boundary tests.
- Decompression cap functions at, below, and above every limit; tests must call
  the enforcement used by production rather than restating constants.

### Offline functional matrix

Build paired NZBs and deterministic yEnc articles against the existing mock
NNTP facilities. At minimum include:

| Group | Scenarios |
|---|---|
| Article | complementary holes, donor absent, ambiguous file, boundary drift, many donors/cache eviction, cutover, bad-lead rotation, proactive count truth |
| Same-byte stream | different segmentation, renamed repost, same size/wrong bytes, too little probe data, partial repair, full repair without PAR2 |
| Cross-packing | bare/split/store-ZIP/copy-7z/store-RAR positives plus compressed/forged/overflow negatives |
| Encryption | both directions, different passwords, wrong password, trailing partial block, missing volume |
| Decompression | compressed formats, encrypted donor, encrypted target, option off, no tool, bomb/cap, symlink/hardlink/traversal, cancellation |
| Live | proven overlap, stream gate, last-file gate, direct-unpack conflict, rename race, delete/shutdown |
| Persistence | restart before fallback, during staged article, before stream, during partial stream, after live attempt, before accounting |

Every positive scenario must assert final bytes, status, recovery metrics, and
absence of residual temp files. Every negative scenario must assert unchanged
known-good target regions and no false success, not merely a log line.

Add an Arr contract scenario: submit A, wait for its terminal failure, submit B
with the same duplicate identity, assert B can use A as a donor, and assert A
never re-enters `Queued`/`Downloading` or emits a second terminal event while
`retry_original_nzb=false`.

When late retry is implemented, add the opt-in inverse: A remains an immutable
failed history event, A#2 has a distinct attempt record, cancellation suppresses
late completion, and output/API events cannot be mistaken for two independent
successful downloads.

### CI and hardening

- Run the article and M1 functional smoke matrix on every pull request.
- Run the full recovery matrix on main/nightly or a dedicated immutable CI
  image containing required archive tools.
- Run Linux ASan/UBSan-equivalent tooling available to Rust (sanitizer builds,
  Miri for suitable pure components, Loom for lifecycle models where useful),
  plus `cargo-fuzz` corpora for all untrusted container parsers.
- Build Windows, Linux amd64/arm64, and desktop targets before enabling M3/M4.
- Add a manual real-provider soak only as supplementary evidence; never make CI
  depend on Usenet credentials.

## Rollout and rollback

1. Ship schema/model foundations with mode `off` and no UI control.
2. Enable `article` for opt-in testers; record metrics and retain a one-click
   rollback to `off`.
3. Expose `stream` only after M1 restart and corruption testing passes.
4. Keep M2 and M3 behind their own experimental build/runtime gates until
   their parser/crypto reviews pass.
5. Keep decompression behind both `stream` capability and its explicit boolean;
   cap resource use conservatively.
6. Enable `live` last, initially experimental, because it adds lifecycle risk
   but no new recovery capability over post-download stream repair.
7. Enable `retry_original_nzb` separately after Arr contract tests pass. Its
   rollout and rollback must not change ordinary donor fallback behavior.

SQLite migrations should be additive. On rollback, old rustnzbd releases may
ignore added columns/tables but must retain queue/history core rows. Test an
upgrade, active-recovery restart, and binary rollback against a copied database
before each release. Never delete old recovery columns in the same release
line.

## Definition of done

The complete port is done only when:

- `dupe_article_fallback` and `retry_original_nzb` behave as documented and
  both default to off;
- duplicate discovery works across queue and retained history NZBs without an
  external file-retention setting;
- donor fallback alone never resurrects a job that already emitted terminal
  failure;
- an enabled original-NZB retry creates a bounded, explicit retry attempt
  rather than mutating or silently reopening the original history item;
- article fallback never charges a terminal failure while a pinned source
  remains;
- every donor write is geometry-valid and, for stream modes, content-proven;
- no partial repair can produce false success;
- recovery survives restart and shuts down/cancels cleanly;
- PAR2 remains available as final verification/repair;
- archive/crypto parsers pass adversarial, fuzz, cross-platform, and security
  review gates;
- recovery statistics are durable and consistent across REST, SAB, history,
  Angular, logs, and telemetry; and
- `off` matches the pre-port functional and performance baseline.

## Suggested pull-request sequence

1. `refactor(dispatch): make nzb-dispatch the sole production dispatcher`
2. `feat(dupes): add duplicate identity and donor catalog`
3. `feat(recovery): add reactive duplicate article fallback`
4. `feat(recovery): validate and persist decoded segment geometry`
5. `feat(recovery): add article cutover and lead rotation`
6. `feat(recovery): add persisted hole ledger and bounded donor fetcher`
7. `feat(recovery): add same-byte post-download stream repair`
8. `feat(recovery): add store/copy content maps`
9. `feat(recovery): add password-assisted store-rar repair`
10. `feat(recovery): add bounded decompression donor mode`
11. `feat(recovery): add live repair lifecycle`
12. `feat(recovery): add opt-in original-NZB retry state machine`
13. `feat(ui): expose duplicate recovery settings and statistics`

Each PR should include its own migration, tests, documentation, and disabled or
fully usable product surface. Avoid merging scaffolding that changes runtime
behavior before its fail-closed tests exist.

## Upstream reference map

Use these only as behavioral references at the reviewed revision; implement
cleanly in Rust under rustnzbd's license:

- Article matching/source order: [`DupeArticleFallback`](https://github.com/nzbgetcom/nzbget/tree/4c3d194c36af1f0062bc91e7618ff3c2d77dc006/daemon/queue)
- Download/write hooks: [`ArticleDownloader.cpp`](https://github.com/nzbgetcom/nzbget/blob/4c3d194c36af1f0062bc91e7618ff3c2d77dc006/daemon/nntp/ArticleDownloader.cpp), [`ArticleWriter.cpp`](https://github.com/nzbgetcom/nzbget/blob/4c3d194c36af1f0062bc91e7618ff3c2d77dc006/daemon/nntp/ArticleWriter.cpp)
- Stream orchestration: [`StreamRepair.cpp`](https://github.com/nzbgetcom/nzbget/blob/4c3d194c36af1f0062bc91e7618ff3c2d77dc006/daemon/postprocess/StreamRepair.cpp)
- Hole geometry: [`DupeStreamRepair.cpp`](https://github.com/nzbgetcom/nzbget/blob/4c3d194c36af1f0062bc91e7618ff3c2d77dc006/daemon/queue/DupeStreamRepair.cpp)
- Content maps and crypto: [`ContentMap.cpp`](https://github.com/nzbgetcom/nzbget/blob/4c3d194c36af1f0062bc91e7618ff3c2d77dc006/daemon/postprocess/ContentMap.cpp), [`StreamCrypto.cpp`](https://github.com/nzbgetcom/nzbget/blob/4c3d194c36af1f0062bc91e7618ff3c2d77dc006/daemon/postprocess/StreamCrypto.cpp)
- Functional behavior matrix: [`tests/functional/dupefallback`](https://github.com/nzbgetcom/nzbget/tree/4c3d194c36af1f0062bc91e7618ff3c2d77dc006/tests/functional/dupefallback)
