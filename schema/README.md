# Protocol Schema

This directory contains the authoritative Protobuf contracts shared by the
TypeScript OpenCode plugin and the native Rust daemon. The live transport is a
small length-delimited Protobuf protocol over a private Unix-domain socket. The
schemas do not declare a gRPC `service`.

## Layout

| Contract                                                      | Protobuf package            | Source                                   |
| ------------------------------------------------------------- | --------------------------- | ---------------------------------------- |
| Generic memory operations and JSON-like values                | `opencode.memory.v1`        | `opencode/memory/v1/memory.proto`        |
| Model catalog and model-switch operations                     | `opencode.memory.model.v1`  | `opencode/memory/model/v1/model.proto`   |
| Knowledge-graph operations and durable extraction jobs        | `opencode.memory.graph.v1`  | `opencode/memory/graph/v1/graph.proto`   |
| Daemon lifecycle, sessions, leases, and project-call envelope | `opencode.memory.daemon.v1` | `opencode/memory/daemon/v1/daemon.proto` |

`daemon.proto` imports the other three contracts and carries exactly one memory,
model, or graph domain request per project call.

## Compatibility Values

These values describe different layers and must not be conflated:

| Value                          | Current value | Meaning                                                                      | Source of truth                                             |
| ------------------------------ | ------------: | ---------------------------------------------------------------------------- | ----------------------------------------------------------- |
| Memory RPC protocol version    |           `2` | Generic memory protocol; version 2 replaced JSON-lines with Protobuf framing | `src/rpc.rs`                                                |
| Daemon protocol generation     |           `1` | Request-envelope and session protocol negotiated by client and daemon        | `src/daemon/mod.rs`, `opencode-memory/src/daemon-client.ts` |
| Domain schema generation       |           `4` | Exact compatibility generation for memory, model, and graph messages         | `src/daemon/mod.rs`, `opencode-memory/src/daemon-client.ts` |
| Persistent memory state schema |           `4` | On-disk lifecycle state format; older versions are rejected                  | `src/storage/state.rs`                                      |
| Persistent graph state schema  |           `1` | On-disk knowledge-graph format                                               | `src/graph/mod.rs`                                          |
| Embedding-generation manifest  |           `1` | Per-generation embedding metadata and lifecycle state                        | `src/embedding_generation.rs`                               |
| Model-switch journal format    |           `1` | Durable switch progress, cancellation, and history                           | `src/embedding_generation.rs`                               |
| Model catalog version          |           `1` | Built-in model-profile catalog format                                        | `src/model.rs`                                              |

The plugin and daemon package versions must also match for a release. Package
version compatibility does not replace protocol or domain-generation checks.
When an incompatible daemon is already running, the client reports its endpoint,
PID, package version, and generation values rather than starting a second daemon
or silently downgrading the client.

## Wire Framing

Every daemon request and response is encoded as:

```text
unsigned varint payload_length
protobuf payload bytes
```

The payload is `DaemonRequest` or `DaemonResponse`. Frames may be split across
socket reads or multiple frames may arrive in one read. Rust applies the 32 MiB
request limit to the Protobuf payload. The TypeScript outbound client checks the
complete length-delimited buffer, including the varint prefix, so its maximum
payload is slightly smaller. Response decoding uses the declared payload length
and both sides enforce a 32 MiB response limit.

`DaemonRequest.request_id` correlates the outer transport response.
`DaemonRequest.protocol_generation` is validated before dispatch. Domain request
IDs are separate `uint64` values inside `Request`, `ModelRequest`, or
`GraphRequest`, and a domain response must return the same ID.

## Proto3 Presence Rules

The comments marked `required` in these files are application-level validation,
not Protobuf `required` fields. Proto3 wire defaults still apply:

| Field form        | Absence behavior                                       |
| ----------------- | ------------------------------------------------------ |
| Ordinary scalar   | Decodes as `""`, `false`, `0`, or enum value `0`       |
| `optional` scalar | Preserves whether the caller supplied the field        |
| Message field     | Has presence; generated Rust normally uses `Option<T>` |
| `repeated` field  | Decodes as an empty collection                         |
| `map` field       | Decodes as an empty map                                |
| `oneof`           | Contains one selected case or no case                  |

All enums reserve numeric zero for `*_UNSPECIFIED`. Application validation
rejects unspecified values where a concrete operation, state, outcome, or status
is required.

The generic `Value` type represents null by selecting the `null_value` oneof
case. Its boolean payload is not semantically meaningful. The TypeScript encoder
maps value-level `null` and `undefined` to that case and omits object properties
whose value is `undefined`.

## Memory Contract

`opencode/memory/v1/memory.proto` keeps the stable generic memory surface. It
uses a typed envelope and a recursively typed JSON-like value tree so the public
tool contract can remain snake-case JSON without transporting JSON text.

### Methods

| Number | Enum                     | TypeScript method |
| -----: | ------------------------ | ----------------- |
|    `1` | `METHOD_SEARCH`          | `search`          |
|    `2` | `METHOD_STORE`           | `store`           |
|    `3` | `METHOD_GET`             | `get`             |
|    `4` | `METHOD_LIST`            | `list`            |
|    `5` | `METHOD_UPDATE`          | `update`          |
|    `6` | `METHOD_PIN`             | `pin`             |
|    `7` | `METHOD_LOCK`            | `lock`            |
|    `8` | `METHOD_DELETE`          | `delete`          |
|    `9` | `METHOD_FORGET`          | `forget`          |
|   `10` | `METHOD_PURGE`           | `purge`           |
|   `11` | `METHOD_FEEDBACK`        | `feedback`        |
|   `12` | `METHOD_SYNC_SHARED`     | `sync_shared`     |
|   `13` | `METHOD_STATUS`          | `status`          |
|   `14` | `METHOD_OPTIMIZE`        | `optimize`        |
|   `15` | `METHOD_DOCTOR`          | `doctor`          |
|   `16` | `METHOD_SHUTDOWN`        | `shutdown`        |
|   `17` | `METHOD_CAPTURE`         | `capture`         |
|   `18` | `METHOD_EXPORT`          | `export`          |
|   `19` | `METHOD_IMPORT`          | `import`          |
|   `20` | `METHOD_INGEST`          | `ingest`          |
|   `21` | `METHOD_INDEX_DOCUMENTS` | `index_documents` |

Values `22` through `25` and their former model-method names are reserved.
Model operations now use the typed `model.proto` branch and those numbers or
names must not be reused.

### Generic Values

`Value` supports booleans, signed and unsigned integers, finite doubles,
strings, lists, objects, and null. Implementations enforce these additional
rules:

- Request IDs must be nonzero safe integers on the TypeScript boundary.
- JavaScript integers outside the safe integer range are rejected.
- Non-finite floating-point values are rejected.
- Nested values are limited to a depth of 64.
- Missing memory request parameters are treated as an empty object by Rust dispatch.

`Response.ok=false` carries a human-readable domain error in `Response.error`.
The outer daemon call may still have `DAEMON_STATUS_CODE_OK` because transport
and memory-domain success are separate layers.

`METHOD_SHUTDOWN` terminates only the legacy standalone protocol loop. Shared
daemon project calls reject it with `FAILED_PRECONDITION`; a project client may
not stop the user daemon globally.

## Model Contract

`opencode/memory/model/v1/model.proto` provides typed model-profile and switch
operations. `ModelRequest.operation` and `ModelResponse.result` use matching
oneof tags:

|  Tag | Request/result      | Public method         | Current support                                |
| ---: | ------------------- | --------------------- | ---------------------------------------------- |
| `10` | `list_profiles`     | `model_profiles`      | Supported and read-only                        |
| `11` | `start_switch`      | `model_switch`        | Dry-run publicly; durable APPLY natively       |
| `12` | `get_switch_status` | `model_switch_status` | Supported natively and publicly                |
| `13` | `cancel_switch`     | `model_switch_cancel` | Cooperative cancellation natively and publicly |

The profile catalog distinguishes modality, metric, support level, role, and
capability. Artifact and sizing fields are optional because preview or
unsupported profiles may not have a locked downloadable artifact.

`StartModelSwitchRequest` also supports `expected_active_generation_id` for
source fencing, `retain_previous` for keeping the predecessor generation, and
`target_generation_id` for an explicit retained-generation rollback. Apply
requests require a `switch_id`; dry-run requests may omit it.

`StartModelSwitchRequest.execution_mode=DRY_RUN` returns typed blockers,
warnings, estimated resources, and dense-search availability. `APPLY` creates a
durable model-switch journal and returns immediately. The actor then loads the
target artifact, prepares a separate generation, reindexes in bounded batches,
verifies IDs and record count, and commits the active-generation pointer.
`retain_previous=true` retains the predecessor generation for a later explicit
rollback request through `target_generation_id`.

`GetModelSwitchStatusResponse` includes progress fraction, record counts,
cancel-request state, dense-search availability, timestamps, and an optional
typed error. `CancelModelSwitchResponse` reports whether cancellation was
requested, completed before commit, already committing, already committed,
terminal, or not found. The public plugin asks permission before apply and
cancel operations; native protocol callers are responsible for their own policy.

Model responses use `ModelStatus`. A non-OK status has no successful result.
The TypeScript decoder verifies that the returned result branch matches the
requested operation.

## Graph Contract

`opencode/memory/graph/v1/graph.proto` contains source-backed graph operations,
provenance, temporal search, and durable leased extraction jobs.

Every graph operation requires non-empty, bounded `GraphAuthorization` session
and agent scope-key values. The official TypeScript plugin supplies the current
OpenCode context keys, but the native daemon currently treats them as scope
selectors and does not independently bind them to the daemon session. The daemon
derives authoritative project, memory scope, and verified scope keys from source
memory; caller-supplied source labels cannot expand visibility.

### Operation Map

|  Tag | Request/result            | TypeScript method                                                      | Mutation class              |
| ---: | ------------------------- | ---------------------------------------------------------------------- | --------------------------- |
| `10` | `extract_prepare`         | `graph_extract_prepare`                                                | Read-only preparation       |
| `11` | `upsert_candidates`       | `graph_upsert_candidates`                                              | Idempotent graph mutation   |
| `12` | `run_status`              | `graph_run_status`                                                     | Read-only status            |
| `13` | `search`                  | `graph_search`                                                         | Read-only search            |
| `14` | `status` / `graph_status` | `graph_status`                                                         | Read-only status            |
| `15` | `export`                  | `graph_export`                                                         | Read-only paginated export  |
| `16` | `extract_enqueue`         | `graph_extract_enqueue`                                                | Durable job creation        |
| `17` | `extract_claim`           | `graph_extract_claim`                                                  | Lease acquisition           |
| `18` | `extract_renew`           | `graph_extract_renew`                                                  | Lease renewal               |
| `19` | `extract_finish`          | `graph_extract_complete`, `graph_extract_fail`, `graph_extract_finish` | Durable terminal transition |
| `20` | `extract_job_status`      | `graph_extract_job_status`                                             | Read-only status            |
| `21` | `extract_cancel`          | `graph_extract_cancel`                                                 | Cooperative cancellation    |

Graph sources bind memory ID, source-unit ID, content hash, extraction revision,
derived scope, origin, egress-policy revision, and remote eligibility. The
daemon revalidates those fields against current memory immediately before graph
commit or provider work.

Candidate evidence contains an exact quote and may include UTF-8 byte offsets.
When both offsets are present, their byte slice must align to UTF-8 boundaries
and select the exact quote. If either offset is absent, the current
implementation requires the quote to occur in the referenced source unit and
may retain a partial offset. Candidate confidence must be finite and between 0
and 1.

Allowed relation predicates are:

```text
uses
depends_on
implements
causes
related_to
supports
contradicts
```

### Durable Jobs

Graph extraction jobs use these principal states and transitions:

```text
queued -> claimed
queued -> cancelled
queued -> failed
claimed -> running
claimed/running -> completed
claimed/running -> failed
claimed/running -> cancelled
claimed/running -> queued       (retryable failure or lease recovery)
```

Important semantics:

- `job_id` is the enqueue idempotency key. Claim replay requires the same `claim_request_id` and `worker_id` while the original claim remains active and visible.
- An identical repeated enqueue returns the existing job.
- Reusing an ID with different material fails.
- `max_attempts=0` defaults to 3; explicit values must be 1 through 5 and larger values are rejected.
- `lease_ttl_ms=0` defaults to 60 seconds and accepted values are bounded.
- A lease token scopes renew and finish operations to one active claim.
- Retryable failure returns the job to the retry schedule until attempts are exhausted.
- Cancellation is immediate for queued work and cooperative for claimed/running work.
- Job records persist source bindings and hashes, not source text.
- Completion commits the run receipt and graph facts atomically and idempotently.

`GraphExtractCancelResponse.outcome` is currently a string with one of
`cancelled`, `cancel_requested`, or `already_terminal`.

### Search And Export

Graph search uses lexical seeding plus bounded traversal. Zero-valued search
limits select native defaults; callers cannot request unbounded traversal.
Current public tools cap depth at 2, fanout at 32, results at 64, and evidence at
8 records per fact.

`GraphTimeFilter` uses inclusive lower and upper bounds. Equal
`valid_after_ms` and `valid_before_ms` values represent an exact as-of instant.
The current implementation first requires the stored relation status to be
`active`, then applies inclusive `valid_at_ms` and exclusive `invalid_at_ms`
bounds. Without an exact as-of pair, search additionally requires current
validity.

Status, search, and export are source-visibility aware. Facts whose supporting
memory is expired, superseded, stale, hidden from the current scope, or no
longer revision-compatible are filtered from reads. Export uses a bounded
cursor page and returns source provenance separately from facts.

## Daemon Contract

`opencode/memory/daemon/v1/daemon.proto` is the transport and lifecycle envelope.
It does not expose project domain methods directly; it carries one typed domain
request inside `ProjectCallRequest`.

### Session Flow

```text
GetDaemonInfo
  -> report protocol range, domain schema, capabilities, version, and PID
  -> client validates protocol overlap, domain schema, and package version
OpenSession
  -> daemon instance ID, session ID, heartbeat interval, lease TTL
AcquireProject
  -> canonical project handle and lease
ProjectCall / CancelCall
ReleaseProject
```

Every session, heartbeat, project lease, call, release, and drain request is
bound to the current daemon instance. IDs from an older daemon process cannot be
reused after restart.

`GetDaemonInfo` and `RequestDrain` are stable message branches, but the daemon
validates the outer protocol generation before dispatching either branch, so
they cannot be used across incompatible outer generations. Drain is accepted
only when there are no active sessions and no project actor with a lease or
queued/active actor work. Persisted queued or retry-scheduled graph jobs do not
by themselves block drain or idle exit. Clients do not kill a busy incompatible
daemon or unlink its live socket.

### Project Calls

`ProjectCallRequest` must contain exactly one of:

```text
request        opencode.memory.v1.Request
model_request  opencode.memory.model.v1.ModelRequest
graph_request  opencode.memory.graph.v1.GraphRequest
```

The corresponding response must contain exactly one matching response branch.
`call_id` supports cancellation and outcome diagnostics independently of the
domain request ID. `timeout_ms` must be nonzero and no greater than the daemon's
two-hour maximum.

### Status Layers

There are three distinct error layers:

| Layer                      | Type                                   | Meaning                                                                         |
| -------------------------- | -------------------------------------- | ------------------------------------------------------------------------------- |
| Daemon transport/lifecycle | `DaemonStatus`                         | Framing, protocol, session, lease, deadline, cancellation, or admission failure |
| Memory domain              | `Response.ok` and `Response.error`     | Generic memory operation success/failure                                        |
| Model/graph domain         | `ModelStatus` / `GraphOperationStatus` | Typed domain operation status                                                   |

`DAEMON_STATUS_CODE_OUTCOME_UNKNOWN` means a mutating call may have committed
before its response was lost. Callers must reconcile using durable receipts or
operation-specific status; they must not blindly replay arbitrary mutations.

## Generated Bindings

Rust bindings are generated into Cargo's build output by `prost-build` from
`build.rs`. They are included by the Rust crate at compile time and are not
committed.

TypeScript bindings are generated with `@bufbuild/protoc-gen-es` and committed at:

```text
opencode-memory/src/generated/opencode/memory/v1/memory_pb.ts
opencode-memory/src/generated/opencode/memory/model/v1/model_pb.ts
opencode-memory/src/generated/opencode/memory/graph/v1/graph_pb.ts
opencode-memory/src/generated/opencode/memory/daemon/v1/daemon_pb.ts
```

Regenerate them after any `.proto` change:

```sh
bun run generate:protocol
```

Verify that committed output is current without modifying it:

```sh
bun run generate:protocol:check
```

Lint all schemas:

```sh
bun run lint:proto
```

Run the TypeScript/Rust interoperability test after protocol changes:

```sh
bun run build:native
bun run test:protocol
```

## Evolution Rules

Follow these rules for every schema change:

1. Never renumber an existing field or enum value.
2. Never reuse a removed field number, field name, enum number, or enum name; reserve it.
3. Add new oneof branches with new tags and keep existing branches stable.
4. Treat comments marked `required` as validation requirements in both implementations.
5. Keep additive fields wire-compatible, but do not require older peers to understand new semantics unless the domain generation is bumped; generated Rust bindings do not promise unknown-field round trips.
6. Bump the daemon protocol generation only when the transport/session envelope is incompatible.
7. Bump the domain schema generation when memory, model, or graph peers must reject older domain semantics. Domain generation 4 includes additive model-switch fields and durable apply/status/cancellation semantics; older peers must not be assumed to understand those semantics.
8. Bump persistent state schema versions only with an explicit migration or documented rejection policy.
9. Bump and synchronize release versions whenever a published client/native compatibility tuple changes.
10. Regenerate TypeScript bindings and run Buf lint, generated-file checks, typecheck, Rust tests, and protocol E2E tests.

Buf is configured in `buf.yaml` with the `STANDARD` lint profile and `FILE`
breaking-change policy. Compare against the intended release baseline before
publishing a protocol change.

## Verification Checklist

```sh
bun run generate:protocol:check
bun run lint:proto
bun run typecheck
bun run test:ts
cargo test --locked --lib
bun run build:native
bun run test:protocol
bun run format:check
```
