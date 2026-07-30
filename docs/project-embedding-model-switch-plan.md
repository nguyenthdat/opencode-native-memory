# Project Embedding Model Switch Plan

Status: Implemented on 2026-07-30; large-model fault-injection E2E remains a release gate

Research snapshot: 2026-07-28

Target: allow one project to switch its active embedding profile through an
explicit command or tool without mixing incompatible vectors, restarting the
shared daemon, invalidating existing project leases, or losing rollback.

Related plans:

- [Shared Daemon Migration Plan](./shared-daemon-migration-plan.md)
- [Multi-Model and Multimodal Embedding Plan](./multi-model-embedding-plan.md)
- [Qwen3-VL Single-Model Embedding Plan](./qwen3-vl-single-model-plan.md)

## Executive Decision

Implement model switching as a durable project migration, not as an environment
variable change and not as a daemon restart.

The recommended design combines:

- one immutable model-profile catalog owned by the daemon;
- one persisted active embedding profile per project;
- one zvec collection generation per profile migration;
- one atomic active-generation pointer;
- one daemon-owned switch job with progress, cancellation, resume, and rollback;
- one project actor that remains the sole writer-lock and collection owner;
- one daemon-wide model-worker supervisor that loads profiles on demand and
  enforces the resident-memory budget;
- an OpenCode custom tool as the mutation API;
- `/memory model switch <profile>` as a prompt command that directs the model to
  the custom tool;
- a separate deterministic CLI for users and automation that must not depend on
  an LLM tool call.

The command starts a job and returns immediately. The existing project remains
on its old generation until the target generation is complete, verified, and
atomically selected.

The first release freezes vector-affecting project mutations during the switch.
It does not implement dual-write or cross-generation result fusion. That keeps
the initial migration recoverable and avoids a second consistency system.

## Why Environment Switching Is Insufficient

The current configuration is discovered from environment variables such as:

```text
OPENCODE_MEMORY_EMBEDDING_MODEL_PATH
OPENCODE_MEMORY_EMBEDDING_MODEL_REPO
OPENCODE_MEMORY_EMBEDDING_MODEL_FILE
OPENCODE_MEMORY_EMBEDDING_MODEL_REVISION
OPENCODE_MEMORY_EMBEDDING_DIMENSION
```

Those values are copied into `EmbeddingConfig` and included in the project
configuration fingerprint. The daemon registry rejects an existing project
actor when another client presents a different fingerprint.

The current zvec manifest also persists the embedding model, dimension, and
configuration fingerprint. Its schema has one fixed-dimension vector field.
Changing an environment variable therefore produces one of these failures:

- the live actor rejects the new client configuration;
- the collection manifest rejects the model or dimension mismatch;
- an unsafe implementation writes incompatible vectors into the old
  collection;
- a daemon restart leaves the project unable to open until it is reindexed.

An environment variable may select the initial default for a new project or
configure daemon runtime policy. It must not silently mutate an existing
project's active embedding profile.

## Decision Record

Pressure: model identity, preprocessing, dimension, and runtime can change at
runtime, while project store ownership, record IDs, lifecycle state, and normal
tool APIs must remain stable.

Decision: represent a switch as a durable command and an explicit migration
state machine. Build a new immutable collection generation beside the current
one, then atomically replace the active pointer.

Language form: Rust enums for job states and profile variants, typed Protobuf
domain methods for control, actor messages for collection ownership, and a
bounded channel between the switch coordinator, project actor, and model
worker.

Ownership: the project actor owns writer lock, logical snapshot, journals,
target collection, and cutover. A daemon-owned coordinator owns job progress
and orchestration. The model supervisor owns model workers. The TypeScript
plugin only starts, polls, and cancels jobs.

Alternatives:

- Environment switch plus daemon restart is rejected because it does not
  perform reindexing and disrupts every project.
- In-place collection rebuild is rejected because failure destroys rollback
  and may expose mixed vectors.
- Dual-write is deferred because it adds change-journal, ordering, retry, and
  cross-generation consistency before the simpler frozen-write flow exists.
- Querying old and new generations together is rejected because their vectors
  may be incomparable and switching is replacement, not multi-space retrieval.
- Running the migration inside the OpenCode plugin is rejected because plugin
  disposal, session lifetime, or a transport error would orphan the operation.

Costs:

- temporary disk use for both old and target collections;
- temporary model-memory pressure or bounded dense-search downtime;
- a new state journal and collection-generation layout;
- protocol and domain schema changes;
- mutation freeze during the first implementation;
- model artifacts may need downloading before the job starts.

Invariants:

1. Exactly one collection generation is active per project.
2. Only the active generation serves normal retrieval and writes.
3. Incompatible vectors never share a collection.
4. A pre-commit failure or cancellation cannot change the active pointer.
5. Existing project handles and leases remain valid across a successful switch.
6. The switch survives client disconnect and daemon restart.
7. Every target vector matches the target profile ID and dimension.
8. Rollback never exposes a stale generation as current data.
9. At most one switch job runs per project.
10. Other projects remain responsive during a switch.

## Current Repository Constraints

### Actor acquisition binds model configuration

The TypeScript daemon client currently sends `EmbeddingIdentity` on every
`AcquireProject` request. The native daemon converts it into `EmbeddingConfig`,
computes a full configuration fingerprint, and passes it to
`ProjectRegistry::acquire()`.

`ProjectRegistry` is keyed by canonical project store path. When an actor
already exists, a different fingerprint is rejected before lease acquisition.

This is correct for the current immutable model design, but model switching
requires the fingerprint to be split into:

```text
ActorCompatibilityFingerprint
  project store identity
  storage schema generation
  daemon protocol compatibility
  writer ownership policy

EmbeddingProfileId
  model artifacts
  runtime adapter
  preprocessing
  prompt templates
  pooling
  normalization
  vector dimension
  distance metric
```

The actor fingerprint remains stable across a profile switch. The profile ID is
owned by the active collection generation.

### Project actors own model and collection today

`ProjectActor::spawn()` currently initializes one `Service`, which initializes
one `MemoryEngine`, one embedder, and one zvec collection before the actor
becomes ready.

The actor processes one synchronous command at a time. If an entire reindex ran
inside one normal project call:

- status and cancel could not run until the call completed;
- client timeout would make the outcome ambiguous;
- the actor queue would be unavailable for hours on a large project;
- a plugin disconnect could not express durable ownership;
- the global inference lock could block other projects.

The switch must be incremental and daemon-owned. The project actor continues to
own every collection mutation but handles reindex batches as short internal
commands that yield between batches.

### One zvec collection has one dimension

The current `collection_schema()` creates one `embedding` field with a fixed
dimension and cosine HNSW index. The manifest rejects a different model,
dimension, or vector-affecting fingerprint.

A switch always creates a different collection generation, even when two
profiles happen to emit the same number of dimensions. Equal dimensions do not
prove vector compatibility.

### Current journals do not identify a generation

Pending upserts currently contain computed document data and vectors, but do
not identify:

- embedding profile ID;
- active collection generation;
- target collection generation;
- switch job ID;
- source record revision.

Generation identity must be present in pending writes before a second
collection can be built safely.

### The plugin already owns `/memory`

The plugin registers one `memory` command in its `config` hook. OpenCode passes
everything after `/memory` as command arguments. There is no real hierarchical
command registry; `model switch` is an argument convention implemented by the
prompt and tools.

The plugin also has automatic recall, capture, shared-memory sync, and document
indexing hooks. Those paths must respect switch admission and must not issue
mutations while the first-release migration freeze is active.

## Terminology

### ModelProfile

An immutable, daemon-approved model configuration:

```text
profile_id
repository and immutable revision
artifact SHA-256 values
runtime family and version
preprocessing version
query and passage instruction policy
pooling and normalization
output dimension and metric
modality support
resource estimate
platform support
```

Production commands select a `profile_id`, not arbitrary repository URLs or
local code.

### EmbeddingGeneration

A project-local zvec collection created for exactly one profile and one source
state snapshot. Generations are immutable after they stop being active, except
for a controlled catch-up before rollback.

### ActiveEmbedding

The atomically persisted pointer that selects one complete generation for
normal project operations.

### ModelSwitchJob

A durable daemon-owned migration from one active generation to one target
profile and target generation.

## Target Architecture

```text
OpenCode plugin or CLI
       |
       | MODEL_SWITCH / STATUS / CANCEL
       v
Shared daemon
  +-----------------------+       +-------------------------+
  | ProjectRegistry       |       | ModelWorkerSupervisor   |
  | stable actor leases   |       | profile workers         |
  +-----------+-----------+       | memory admission        |
              |                   | idle eviction           |
              v                   +------------+------------+
  +-----------------------+                    |
  | ProjectActor          |<-------------------+
  | writer.lock owner     |       vectors tagged by profile
  | active collection     |
  | target collection     |<-------------------+
  | cutover barrier       |                    |
  +-----------+-----------+                    |
              ^                                |
              | short internal batches         |
              |                                |
  +-----------+-----------+                    |
  | SwitchCoordinator     |--------------------+
  | durable job journal   |
  | snapshot cursor       |
  | progress and cancel   |
  +-----------------------+
```

### Ownership

- `ProjectRegistry` resolves one actor from one canonical store key.
- `ProjectActor` remains the sole owner of writer lock and all zvec collection
  handles.
- `SwitchCoordinator` never writes zvec directly. It asks the actor for logical
  source batches, asks a model worker for vectors, then submits target batches
  back to the actor.
- `ModelWorkerSupervisor` may share one resident profile worker across projects
  that request the exact same profile ID.
- The plugin does not own job execution. It may disappear after receiving the
  switch ID.
- The active pointer and job journal are authoritative after daemon restart.

### Batch workflow

One reindex batch follows this sequence:

```text
SwitchCoordinator
  -> ProjectActor: read stable logical records after cursor
  <- records + source revisions + next cursor
  -> ModelWorkerSupervisor: embed target profile batch
  <- vectors + profile ID + dimensions
  -> ProjectActor: validate and write target generation batch
  <- durable target checkpoint
  -> persist switch progress
  -> yield before requesting another batch
```

The actor never holds its project queue while model inference is running. It
captures source data in one short operation and commits completed vectors in a
later short operation.

## Per-Project Profile Authority

### Fresh project

For a project without an active generation:

1. `AcquireProject` may provide an initial profile preference.
2. The daemon resolves that profile against its catalog.
3. The selected profile is persisted with the first collection generation.
4. Future acquisition reads the persisted profile.

### Existing project

For a project with an active generation:

- the persisted active profile is authoritative;
- an acquisition request cannot switch it;
- the client may omit `expected_profile_id`;
- a strict client may send `expected_profile_id` and receive a mismatch error
  that includes the actual profile ID;
- reconnecting with stale environment defaults does not replace the active
  project profile;
- only `MODEL_SWITCH` may create and activate a different generation.

### Protocol adjustment

Replace client-owned model mutation semantics with expectation semantics:

```proto
message AcquireProjectRequest {
  string daemon_instance_id = 1;
  string session_id = 2;
  string project_root = 3;
  string worktree = 4;
  optional string data_dir = 5;
  optional string initial_profile_id = 6;
  optional string expected_profile_id = 7;
  optional string model_cache = 8;
}
```

Field numbers are illustrative. Existing beta fields must be reserved and
migrated deliberately.

Daemon-wide runtime policy remains global:

- model profile catalog;
- allowed artifact sources;
- resident-memory budget;
- maximum model workers;
- device policy;
- download and load timeouts;
- worker sandbox policy.

Active profile selection is project-scoped.

## Storage Layout

```text
projects/<project-id>/
  writer.lock
  state.json
  document-index.json
  pending/
  manifest.json                    # current legacy generation before migration
  zvec/                            # current legacy collection before migration
  active-embedding.json
  model-switch.json
  embedding-generations/
    <generation-id>/
      manifest.json
      zvec/
      checkpoints/
```

### Active pointer

`active-embedding.json` contains:

```text
format_version
project_id
generation_id
profile_id
profile_fingerprint
embedding_dimension
activated_at_ms
predecessor_generation_id
source_state_revision
```

Install it with the existing safe pattern:

1. create a new temporary file in the same directory;
2. write complete JSON and trailing newline;
3. `fsync` the file;
4. rename atomically;
5. `fsync` the parent directory where supported.

Normal operations capture the active generation at transaction admission. A
single operation never reads metadata from one generation and vectors from
another.

### Generation manifest

Each target generation records:

```text
format_version
generation_id
project_id
profile_id
profile_fingerprint
artifact_digests
runtime_identity
preprocessing_identity
embedding_dimension
metric
normalization
source_generation_id
source_state_revision
record_count
status
created_at_ms
completed_at_ms
```

Valid generation states are:

```text
building
complete
active
retained
quarantined
deleting
```

An incomplete or quarantined generation is never selected by normal retrieval.

### Switch journal

`model-switch.json` records:

```text
switch_id
idempotency_key
source_generation_id
source_profile_id
target_generation_id
target_profile_id
phase
cursor
completed_records
total_records
cancel_requested
source_state_revision
created_at_ms
updated_at_ms
completed_at_ms
error_code
error_message
```

Only one non-terminal switch journal exists per project. Keep bounded terminal
history for status and diagnostics.

## Switch State Machine

```text
Queued
  -> Validating
  -> Downloading
  -> Preparing
  -> Reindexing
  -> Verifying
  -> Committing
  -> Succeeded
  -> CleanupPending

Queued | Validating | Downloading | Preparing | Reindexing | Verifying
  -> CancelRequested
  -> Cancelled

Any pre-commit state
  -> Failed

Committing
  -> Succeeded | FailedAfterCommitCheck
```

### Queued

- Persist switch ID and target profile.
- Return to the initiating client immediately.
- Hold an internal project-job lease so actor idle eviction cannot stop the
  project.

### Validating

- Resolve the target profile from the daemon catalog.
- Reject the active profile as a no-op unless `force_rebuild` is explicit.
- Validate platform and runtime availability.
- Estimate artifact, disk, and resident-memory requirements.
- Verify expected source generation and project ID.

### Downloading

- Download immutable artifacts into a content-addressed cache.
- Verify every digest before worker load.
- Support bounded progress and cancellation.
- Never execute model-repository code.

### Preparing

- Freeze vector-affecting project mutations.
- Capture source generation and state revision.
- Create a target generation with an incomplete manifest.
- Count or estimate source records for progress.
- Start or acquire the target profile worker.

### Reindexing

- Read logical records in bounded batches.
- Embed with the target profile.
- Validate profile ID, dimension, finiteness, and normalization.
- Write target documents and checkpoints through the project actor.
- Persist progress after every durable batch.
- Yield to interactive actor calls between batches.

### Verifying

- Flush target zvec state.
- Compare target record count and source record IDs.
- Validate all schema and manifest fields.
- Run frozen retrieval probes appropriate to the profile.
- Confirm source state revision did not change during the mutation freeze.
- Mark the generation complete and `fsync` it.

### Committing

- Enter a short project-actor barrier.
- Stop admitting new project calls that depend on active generation.
- Atomically install `active-embedding.json`.
- Update actor active-generation handle.
- Release the barrier.
- This is the point of no return for cancellation.

### Succeeded

- Release the mutation freeze and internal job lease.
- Invalidate plugin recall caches on the next status response or project event.
- Keep the predecessor generation read-only for rollback.
- Schedule old model worker eviction according to normal idle policy.

### Failure

- Keep the old pointer unchanged before commit.
- Quarantine or retain the incomplete target for diagnostics/resume.
- Release the mutation freeze.
- Reload or reacquire the old profile worker if sequential mode unloaded it.
- Return a typed failure with an actionable phase and reason.

## Read and Write Policy

### First release

Allowed during a switch:

- `model_switch_status`;
- `model_switch_cancel` before commit;
- daemon heartbeat and project lease operations;
- `status` without automatic syncing;
- `get`;
- `list`;
- `export`;
- lexical-only search against the old active generation;
- dense search only when the old profile worker can remain within budget.

Rejected with `SWITCH_IN_PROGRESS` and retry guidance:

- `store`;
- `capture`;
- `update`;
- `delete`;
- `forget`;
- `purge`;
- `import`;
- `ingest`;
- `index_documents`;
- `sync_shared`;
- `optimize`;
- any operation that changes active zvec documents or source snapshot.

The plugin must not convert these rejections into an unbounded local queue.
Users receive the switch ID and can retry after completion.

### Automatic plugin work

During a switch:

- automatic capture skips with a structured diagnostic;
- file-watcher document sync defers one bounded rescan until completion;
- shared-memory sync marks itself pending and runs once after completion;
- `memory_status` does not call `syncSharedMemories()` or `indexDocuments()`;
- automatic recall uses lexical-only fallback or returns no injected memory when
  dense search is unavailable;
- successful commit invalidates all recall cache generations.

### Later concurrent-write mode

Concurrent writes may be added with a bounded, persisted change journal and a
`CatchingUp` phase. It is not part of the first implementation.

The later design must prove:

- mutation order is stable;
- replay is idempotent by record ID and revision;
- journal size is bounded;
- delete and update races cannot resurrect stale records;
- the cutover barrier sees an empty durable journal;
- cancellation preserves the old active generation.

## Model Memory Policy

Switching between two large profiles may require more memory than normal
operation. The control plane supports two explicit availability modes.

### Atomic availability mode

```text
availability = keep_old_dense
```

- Reserve enough budget for the target worker and any old worker request during
  migration.
- Keep old dense search available.
- Reject the switch before download/load when the reservation cannot be made.
- Never rely on swap or the OOM killer.

### Sequential availability mode

```text
availability = allow_dense_downtime
```

- Keep the old collection and active pointer intact.
- Allow the old worker to exit before loading the target worker.
- Serve lexical-only search while target reindex runs.
- On pre-commit failure, stop the target worker and reload the old worker.
- Require explicit user consent because semantic recall is temporarily
  unavailable.

The command does not silently choose downtime. The preflight response reports
the selected mode, estimated disk use, estimated resident memory, and expected
dense-search availability.

## Domain API

Use the existing generic `ProjectCall` envelope. No new daemon-level oneof arm
is required for project-scoped switching.

The wire route and execution route are intentionally different:

```text
ProjectCall envelope
  -> validate daemon, session, project handle, lease, and deadline
  -> inspect domain method
  -> switch control router
       -> start/status/cancel actor control message
       -> daemon SwitchCoordinator
  -> normal methods continue to Service::handle
```

`MODEL_SWITCH`, `MODEL_SWITCH_STATUS`, and `MODEL_SWITCH_CANCEL` are not long
synchronous `MemoryEngine` calls. The daemon or actor intercepts them before the
ordinary `Service::handle()` dispatch. Start persists and schedules the job,
then returns. Status reads coordinator or persisted job state. Cancel sets a
durable cooperative cancellation flag.

Add explicit actor control messages rather than encoding internal job work as
fake public requests:

```text
StartSwitch
SwitchStatus
CancelSwitch
ReadReindexBatch
CommitTargetBatch
CommitCutover
```

Status and cancel use a small reserved control capacity so a full ordinary
project queue cannot make an active job unobservable or uncancellable. Internal
reindex batches use the ordinary bounded work budget and yield between batches.
The daemon priority classifier may inspect the inner domain method for switch
status and cancel while retaining the existing outer `ProjectCall` schema.

Append domain methods to `schema/opencode/memory/v1/memory.proto`:

```proto
METHOD_MODEL_SWITCH = 22;
METHOD_MODEL_SWITCH_STATUS = 23;
METHOD_MODEL_SWITCH_CANCEL = 24;
METHOD_MODEL_PROFILES = 25;
```

Exact values depend on the final enum at implementation time.

### List profiles

Request:

```json
{}
```

Response:

```json
{
  "active_profile_id": "qwen3-text-4b-q4",
  "active_generation_id": "gen_01...",
  "profiles": [
    {
      "profile_id": "qwen3-vl-embedding-8b",
      "modalities": ["text", "image", "mixed"],
      "dimension": 2048,
      "installed": false,
      "platform_supported": true,
      "estimated_download_bytes": 16301103329,
      "estimated_resident_bytes": null
    }
  ]
}
```

### Start switch

Request:

```json
{
  "switch_id": "switch_01...",
  "target_profile_id": "qwen3-vl-embedding-8b",
  "expected_active_generation_id": "gen_old",
  "availability": "allow_dense_downtime",
  "retain_previous": true,
  "dry_run": false,
  "force_rebuild": false
}
```

Response:

```json
{
  "switch_id": "switch_01...",
  "state": "queued",
  "active_profile_id": "qwen3-text-4b-q4",
  "target_profile_id": "qwen3-vl-embedding-8b",
  "active_generation_id": "gen_old",
  "target_generation_id": "gen_new",
  "dense_search_available": true,
  "created_at_ms": 0
}
```

`switch_id` is generated client-side and is the idempotency key.

Idempotency rules:

- same switch ID and same request returns the existing job;
- same switch ID with a different target returns `FAILED_PRECONDITION`;
- same target while an equivalent job runs returns that job;
- a different target while a switch runs returns `BUSY`;
- a switch to the active profile returns a no-op unless force rebuild is set.

### Status

Request:

```json
{
  "switch_id": "switch_01..."
}
```

Response:

```json
{
  "switch_id": "switch_01...",
  "state": "reindexing",
  "phase": "reindexing",
  "active_profile_id": "qwen3-text-4b-q4",
  "target_profile_id": "qwen3-vl-embedding-8b",
  "completed_records": 423,
  "total_records": 1900,
  "fraction": 0.2226,
  "cancel_requested": false,
  "dense_search_available": false,
  "created_at_ms": 0,
  "updated_at_ms": 0,
  "completed_at_ms": null,
  "error": null
}
```

Status is side-effect-free and retry-safe. It must not trigger shared sync,
document indexing, model load, or project mutation.

### Cancel

Request:

```json
{
  "switch_id": "switch_01..."
}
```

Possible outcomes:

```text
cancel_requested
cancelled_before_commit
already_committing
already_committed
already_terminal
not_found
```

Cancellation is cooperative between batches. The existing daemon `CancelCall`
is insufficient because it can cancel only a queued project call, not a durable
job after the start call returns.

### Rollback

The initial API does not need a distinct rollback method. Rollback is another
switch whose target is a retained generation or its profile:

```text
MODEL_SWITCH target_generation_id=<retained-generation>
```

A direct pointer rollback is allowed only when the retained generation has the
same canonical source revision. Otherwise the retained generation must catch up
or be rebuilt before cutover.

## OpenCode Command and Tool UX

### Recommended commands

```text
/memory model profiles
/memory model switch qwen3-vl-embedding-8b
/memory model switch qwen3-vl-embedding-8b --allow-dense-downtime
/memory model status switch_01...
/memory model cancel switch_01...
/memory model rollback gen_01...
```

OpenCode registers only the root command `memory`. `model switch` and the flags
are command arguments.

### Command template

The existing plugin `config` hook can extend the command template:

```ts
config.command ??= {};
config.command.memory ??= {
  description: "Inspect and manage project memory",
  template: `Manage memory for the current project. User request: $ARGUMENTS

For "model profiles", call memory_model_profiles.
For "model switch <profile>", call memory_model_switch exactly once, then report the switch ID.
For "model status <switch-id>", call memory_model_switch_status.
For "model cancel <switch-id>", call memory_model_switch_cancel.
Never change embedding environment variables or restart the daemon to switch a project profile.`,
};
```

This is model-mediated. OpenCode custom commands are prompt templates, not
imperative command callbacks. The LLM reads the expanded template and invokes
the plugin tool.

### Custom tools

The plugin should expose:

```text
memory_model_profiles
memory_model_switch
memory_model_switch_status
memory_model_switch_cancel
```

Illustrative start tool:

```ts
memory_model_switch: tool({
  description: "Start a durable project embedding-profile migration.",
  args: {
    profile_id: tool.schema.string().min(1).max(128),
    allow_dense_downtime: tool.schema.boolean().default(false),
    dry_run: tool.schema.boolean().default(false),
  },
  async execute(args, context) {
    await context.ask({
      permission: "memory_model_switch",
      patterns: [args.profile_id],
      always: [],
      metadata: { operation: "model_switch", ...args },
    });
    const switchID = createSwitchID();
    const response = await native.request(
      "model_switch",
      {
        switch_id: switchID,
        target_profile_id: args.profile_id,
        availability: args.allow_dense_downtime ? "allow_dense_downtime" : "keep_old_dense",
        dry_run: args.dry_run,
        retain_previous: true,
      },
      context.abort,
    );
    session.invalidateRecall();
    return result("Embedding model switch", response, response);
  },
});
```

The code is illustrative. The final ID format, contracts, and return types must
be defined centrally and tested.

### `command.execute.before` hook

OpenCode exposes `command.execute.before` with command name, session ID, and raw
arguments. It can validate, audit, or start deterministic plugin work.

It does not provide a documented successful "handled, skip the prompt" result.
After the hook returns, OpenCode still runs the normal command prompt. Throwing
aborts the command as an error.

Therefore:

- do not make this hook the only public switch mechanism;
- do not start the job in the hook and then let the prompt call the tool again;
- use it for argument validation, logging, or adding a non-mutating marker;
- keep the actual start operation in one shared function used by the custom
  tool and deterministic CLI.

### Deterministic CLI

Automation and users who require a non-LLM command need a proposed control CLI:

```text
opencode-memory model profiles --project-root "$PWD"
opencode-memory model switch \
  --project-root "$PWD" \
  --profile qwen3-vl-embedding-8b \
  --allow-dense-downtime
opencode-memory model status --project-root "$PWD" --switch-id switch_01...
opencode-memory model cancel --project-root "$PWD" --switch-id switch_01...
```

This CLI does not exist today. It should connect to the shared daemon and use
the same typed project methods as the plugin. It must not open a second
`MemoryEngine` or writer lock directly.

The CLI start command returns immediately by default. `--wait` polls status.
`--dry-run` performs artifact, disk, platform, and memory preflight without
creating a target generation.

## Retry and Outcome Semantics

The TypeScript client currently retries only read-safe methods such as `get`,
`list`, `status`, `doctor`, and `export` after a dispatched transport failure.

Model switch method classification:

| Method                | Retry policy                                                         |
| --------------------- | -------------------------------------------------------------------- |
| `model_profiles`      | Retry-safe                                                           |
| `model_switch_status` | Retry-safe                                                           |
| `model_switch`        | Not automatically replayed after ambiguous dispatch                  |
| `model_switch_cancel` | Idempotent by switch ID but conservatively not replayed until tested |

The start request remains safe for manual retry because `switch_id` is a
durable idempotency key. If the client receives `OUTCOME_UNKNOWN`, it polls
status with the same switch ID instead of generating a new ID.

The daemon should map expected switch failures to stable typed statuses:

```text
PROFILE_NOT_FOUND
PROFILE_UNSUPPORTED
ACTIVE_PROFILE_MISMATCH
SWITCH_IN_PROGRESS
INSUFFICIENT_DISK
INSUFFICIENT_MEMORY
ARTIFACT_VERIFICATION_FAILED
TARGET_VECTOR_INVALID
SOURCE_CHANGED
CANCEL_TOO_LATE
```

Do not collapse these into an outer generic `INTERNAL` error.

## Concurrency

- At most one model switch runs per project.
- A switch owns an internal job lease independent of client sessions.
- Project actor idle eviction treats the job as active work.
- Daemon drain waits for commit or requests cooperative pre-commit cancel.
- Interactive reads have priority over migration batches.
- Target embedding uses bounded batch size and yields between batches.
- Other projects continue through their own actors.
- Model worker scheduling is fair across projects.
- A global worker-memory budget may pause or reject a switch before commit.
- The cutover barrier is short and owned by the project actor.
- No lock guard is held across an async worker inference request.

## Restart and Recovery

At daemon or actor startup:

```text
no switch journal
  -> open active generation

pre-commit journal + valid target checkpoint
  -> verify source revision and target profile
  -> resume from durable cursor

pre-commit journal + invalid source/profile
  -> mark failed or quarantine target
  -> keep old generation active

committing journal
  -> inspect active pointer
  -> finish idempotent pointer transition or restore old active state

succeeded journal
  -> open selected generation
  -> expose terminal status
  -> schedule predecessor retention cleanup
```

The active pointer is the authority. A journal that says `succeeded` cannot
override a pointer that was never durably installed.

## Rollback and Retention

Keep the predecessor generation for a bounded retention period or until
explicit cleanup.

Rollback cases:

| Case                                 | Action                                                 |
| ------------------------------------ | ------------------------------------------------------ |
| No canonical mutations since cutover | Verify and switch pointer back                         |
| Canonical mutations exist            | Re-embed or catch retained generation up before switch |
| Old profile artifacts missing        | Resolve and verify artifacts before rollback           |
| Old generation corrupt               | Refuse pointer switch and rebuild                      |
| Dimension/profile mismatch           | Treat as a new migration                               |

Cleanup never deletes:

- the active generation;
- the source generation of a running switch;
- a generation retained by an explicit rollback pin;
- a generation needed to recover an ambiguous commit.

## Security

- Select production profiles only by catalog ID.
- Pin repository revision and artifact SHA-256 values.
- Reject arbitrary `trust_remote_code` and runtime arguments.
- Require user permission before starting, cancelling, or rolling back a switch.
- Report download size, expected disk amplification, and availability mode
  before approval.
- Keep worker access separate from project stores and writer locks.
- Bound profile IDs, switch IDs, status history, batches, queue bytes, and job
  durations.
- Reject path traversal and symlinks in local development profile artifacts.
- Do not log memory content in progress or error records.
- Audit actor profile changes with source and target IDs, not raw user data.

## Observability

`memory_status` should expose a compact summary without starting sync work:

```text
active_profile_id
active_generation_id
switch_state
switch_id
target_profile_id
switch_fraction
dense_search_available
resident_worker_profiles
```

`memory_model_switch_status` returns detailed phase timing and progress.

Daemon diagnostics include:

- profile catalog digest;
- active and retained generations;
- incomplete/quarantined generations;
- worker PID, runtime, device, RSS, and idle age;
- artifact download progress;
- last durable batch cursor;
- mutation-freeze state;
- actor job-lease count;
- last switch failure code.

Metrics must not include memory content, query text, file paths outside bounded
project-relative diagnostics, or model repository credentials.

## Rollout Phases

### Phase 0: Profile catalog and preflight

Deliverables:

- immutable built-in profile IDs and artifact locks;
- profile listing and platform support diagnostics;
- disk and resident-memory preflight;
- split actor fingerprint from embedding profile ID;
- make persisted project profile authoritative on acquire.

Exit criteria:

- reconnecting with stale environment defaults opens the persisted project;
- unsupported profiles fail before artifact download;
- no acquisition request can silently mutate an existing project;
- model profile and actor compatibility identities have separate tests.

### Phase 1: Generation storage

Deliverables:

- generation manifests;
- active pointer;
- legacy root collection adapter;
- generation-aware journals;
- retained-generation cleanup guardrails.

Exit criteria:

- one active generation opens deterministically;
- incomplete generations are never searched;
- pointer installation survives crash injection;
- the current collection remains readable as the legacy generation.

### Phase 2: Durable switch job

Deliverables:

- switch journal and Rust state enum;
- coordinator and project internal-job lease;
- bounded source, embedding, and commit batches;
- status, cancellation, and restart recovery;
- first-release mutation freeze.

Exit criteria:

- start returns before reindex completes;
- status and cancel remain available during reindex;
- client disconnect does not cancel the job;
- actor idle eviction does not stop the job;
- failed and cancelled jobs leave the old pointer active.

### Phase 3: Worker integration and cutover

Deliverables:

- target profile worker acquisition;
- memory reservation and availability modes;
- target vector conformance validation;
- frozen retrieval probes;
- short actor cutover barrier;
- predecessor retention and rollback.

Exit criteria:

- worker profile mismatch cannot enter zvec;
- insufficient memory fails before active state changes;
- successful cutover is atomic;
- rollback never exposes stale canonical content;
- other projects stay responsive.

### Phase 4: Plugin tools and command UX

Deliverables:

- model profiles, switch, status, and cancel tools;
- `/memory model ...` command guidance;
- permission prompts and dry-run output;
- switch-aware automatic recall, capture, and document sync;
- recall-cache invalidation after commit.

Exit criteria:

- each command path starts at most one job;
- status polling performs no sync or model load;
- automatic hooks do not mutate during a switch;
- outcome-unknown errors recover through switch ID status;
- existing text-memory commands remain compatible.

### Phase 5: Deterministic CLI and packaging

Deliverables:

- `opencode-memory model` control CLI;
- non-LLM profile switch automation;
- packaged model/runtime profile metadata;
- black-box restart, upgrade, and rollback tests;
- user documentation for disk, RAM, downtime, and cleanup.

Exit criteria:

- CLI and plugin call the same daemon methods;
- CLI never opens project storage directly;
- `--dry-run` makes no project mutation;
- `--wait` reconnects and resumes polling safely;
- packaged upgrades preserve active and retained generations.

## Test Plan

### Identity and acquisition

- actor fingerprint excludes active model profile;
- profile fingerprint includes every vector-affecting setting;
- persisted profile wins over stale client environment;
- optional expected-profile mismatch is actionable;
- two clients share one actor across a completed switch;
- fresh project initial profile is selected once.

### State machine

- every allowed transition;
- every rejected transition;
- duplicate switch ID with same target;
- duplicate switch ID with different target;
- different target while busy;
- cancel in every pre-commit phase;
- cancel during commit;
- no-op active-profile switch;
- force rebuild of the active profile.

### Storage and crash recovery

- crash before generation creation;
- crash during artifact download;
- crash after target batch commit;
- crash during verification;
- crash before pointer rename;
- crash after pointer rename but before journal update;
- corrupted active pointer;
- incomplete or quarantined target;
- generation cleanup interrupted and resumed.

### Concurrency

- reads during reindex;
- rejected writes during mutation freeze;
- automatic capture during switch;
- file watcher during switch;
- shared sync during switch;
- actor eviction while client disconnects;
- daemon drain during reindex and commit;
- switches in two different projects;
- shared target worker fairness;
- memory admission with old and target profiles.

### Protocol and retry

- method enum and generated binding parity;
- domain schema generation mismatch;
- retry-safe profile list and status;
- transport failure before switch dispatch;
- transport failure after switch admission;
- status lookup by the original switch ID;
- cancel replay and terminal outcomes;
- stable typed failure mapping.

### Retrieval and migration quality

- target record ID set equals source set;
- correct target dimensions and normalization;
- no old-profile vectors in target generation;
- frozen text, code, Vietnamese, image, and mixed probes as appropriate;
- same-profile rebuild consistency;
- old generation retrieval after failed switch;
- new generation retrieval after successful switch;
- rollback after no writes and after later writes.

### Plugin and CLI

- `/memory model profiles` invokes only profile listing;
- `/memory model switch` invokes start once;
- permission denial creates no job;
- tool abort before dispatch creates no job;
- plugin disposal after start does not stop the job;
- status does not call `syncProjectIndexes`;
- successful commit invalidates recall caches;
- CLI and tool produce equivalent requests;
- CLI `--dry-run` writes no journal or generation.

## Acceptance Criteria

1. An existing project can switch to a different profile without restarting the
   shared daemon.
2. Existing OpenCode project leases remain valid after cutover.
3. A project has exactly one active generation at every durable point.
4. Old and target vectors never share one zvec collection.
5. The old generation remains active until target verification succeeds.
6. A pre-commit failure or cancellation leaves search on the old generation.
7. Start returns a durable switch ID before long-running work begins.
8. Status survives plugin disconnect and daemon restart.
9. Duplicate start is idempotent by switch ID.
10. At most one switch runs per project.
11. Project actor idle eviction cannot stop a running switch.
12. Other projects remain responsive during reindex.
13. First-release vector mutations are rejected clearly during the freeze.
14. Automatic plugin hooks do not bypass the mutation freeze.
15. Worker memory admission occurs before target model load.
16. Dense downtime requires explicit permission.
17. Target vectors match target profile identity and dimension.
18. Active pointer replacement is atomic and crash recoverable.
19. Rollback does not expose stale canonical state.
20. Production profiles are immutable catalog entries with verified artifacts.
21. The custom command does not rely on changing environment variables.
22. A deterministic non-LLM CLI uses the same daemon control plane.

## Expected Files to Change

### Protocol and generated bindings

- `schema/opencode/memory/v1/memory.proto`
- `schema/opencode/memory/daemon/v1/daemon.proto` for acquisition expectation
  semantics, not for switch job methods
- generated Rust bindings through `build.rs`
- `opencode-memory/src/generated/opencode/memory/v1/memory_pb.ts`
- `opencode-memory/src/generated/opencode/memory/daemon/v1/daemon_pb.ts`
- `opencode-memory/src/protocol.ts`

### Rust daemon and engine

- `src/config.rs`: profile IDs, catalog resolution, and split fingerprints
- `src/contract.rs`: profile and switch request/response contracts
- `src/rpc.rs`: new domain dispatch and proposed control CLI arguments
- `src/daemon/mod.rs`: persisted-profile acquisition and job lifecycle
- `src/daemon/registry.rs`: actor compatibility independent from active profile
- `src/daemon/actor.rs`: internal job lease, batch commands, admission, and
  cutover barrier
- `src/daemon/`: switch coordinator and model worker supervisor integration
- `src/engine/mod.rs`: generation-aware operations and source batching
- `src/engine/retrieval.rs`: capture one active generation per operation
- `src/storage/zvec.rs`: generation paths, manifests, and active pointer
- `src/storage/state.rs`: generation-aware pending writes and switch journal
- `src/document_index.rs`: stable source revisions during reindex

### TypeScript plugin and client

- `opencode-memory/src/daemon-client.ts`: acquisition expectations, method retry
  classification, and switch status polling
- `opencode-memory/src/contracts.ts`: profile and switch types
- `opencode-memory/src/plugin.ts`: tools, command guidance, permission, and
  switch-aware automatic hooks
- `opencode-memory/src/session-context.ts`: active profile and recall cache
  generation invalidation
- package CLI entry point for deterministic `opencode-memory model` commands

### Documentation and tests

- README profile-switch user guide
- profile catalog and hardware support table
- protocol golden-frame and generated-binding tests
- actor, recovery, migration, plugin, CLI, memory-budget, and quality tests

## Primary OpenCode References

- [OpenCode commands](https://opencode.ai/docs/commands/)
- [OpenCode plugins](https://opencode.ai/docs/plugins/)
- [OpenCode plugin API source](https://github.com/anomalyco/opencode/blob/35075bb46692a921ab36715e5e1f4bf7f2def494/packages/plugin/src/index.ts)
- [OpenCode command execution source](https://github.com/anomalyco/opencode/blob/35075bb46692a921ab36715e5e1f4bf7f2def494/packages/opencode/src/session/prompt.ts#L1358-L1482)
- [OpenCode command registry](https://github.com/anomalyco/opencode/blob/35075bb46692a921ab36715e5e1f4bf7f2def494/packages/opencode/src/command/index.ts)
- [OpenCode project bootstrap](https://github.com/anomalyco/opencode/blob/35075bb46692a921ab36715e5e1f4bf7f2def494/packages/opencode/src/project/bootstrap.ts)
- [OpenCode SDK session command API](https://opencode.ai/docs/sdk/#sessions)

## Final Recommendation

Support project-scoped switching, but do not make the OpenCode command hook the
owner of the migration.

The durable operation belongs in the native daemon. The OpenCode layer should
have two fronts over the same control plane:

- a custom tool for agent-driven `/memory model ...` workflows;
- a deterministic CLI for scripts and users who require direct execution.

The persisted active profile, generation manifest, and atomic pointer replace
environment variables as the project source of truth. This makes profile
switching compatible with shared daemon leases, multiple OpenCode sessions,
crash recovery, memory admission, and rollback.
