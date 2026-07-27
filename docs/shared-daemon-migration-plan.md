# Shared Daemon Migration Plan

Status: Proposed

Target: replace the per-OpenCode-process Rust sidecar with a user-scoped daemon that can serve multiple OpenCode processes, including multiple processes working on the same project.

## Decision Summary

1. OpenCode's TypeScript plugin connects directly to the daemon through private local IPC.
2. The daemon uses Protobuf as the wire contract and prefers gRPC over a Unix domain socket as the transport, subject to a Phase 0 feasibility spike.
3. MCP is not part of the plugin-to-daemon path and is not required for this migration.
4. The existing Rust `MemoryEngine` remains the owner of one project's zvec collection, lifecycle state, document index, and embedding context.
5. The daemon owns one `ProjectActor` per canonical physical project store. Actor lookup is keyed only by the resolved store directory; project and embedding settings are validated as a separate configuration fingerprint.
6. The existing `writer.lock` remains as defense in depth. Normal OpenCode processes no longer open the project independently, so they no longer compete for that lock.
7. Client disposal closes a daemon connection or releases a project lease. It must never shut down the shared daemon.
8. Model sharing across different projects is deferred. The first migration guarantees one model instance per active project actor, not one model instance for every OpenCode process.
9. The daemon uses a Tokio multi-thread runtime for IPC/control work and runs blocking engines on bounded thread-affine workers. Different project stores execute concurrently; one store retains a serialized transaction lane.

## Scope

### In scope

- A single user-level daemon serving multiple OpenCode processes.
- Same-project access from multiple OpenCode processes.
- Canonical project identity and one engine owner per project.
- Direct TypeScript-to-daemon IPC.
- Protobuf direct-IPC protocol versioning and generated Rust/TypeScript bindings.
- Daemon bootstrap, reconnect, lease release, idle shutdown, and crash recovery.
- Async IPC, bounded multi-threaded execution, backpressure, and resource governance.
- Preservation of the current MemoryEngine journal and recovery behavior.
- Cross-process tests and packaging changes.

### Out of scope for the first release

- A remote or network-accessible memory service.
- A REST gateway.
- An MCP server or MCP adapter in the request path.
- Sharing one mutable llama.cpp context concurrently across projects.
- Changing the zvec collection schema or lifecycle state schema without a separate migration.
- Removing `writer.lock`.
- Allowing arbitrary third-party clients to access the daemon.

## Current Problem

The current lifecycle is:

```text
OpenCode process
  -> TypeScript NativeMemoryClient
  -> child Rust sidecar over stdin/stdout
  -> one MemoryEngine
  -> one writer.lock
  -> one zvec collection + state + embedding model
```

The client pool in `opencode-memory/src/sidecar-client.ts` is process-local. `globalThis` and `Symbol.for(...)` share clients only inside one JavaScript process; they cannot coordinate separate OpenCode processes.

The first request performs the following sequence:

```text
status handshake
  -> Service::engine()
  -> MemoryEngine::open()
  -> acquire writer.lock
  -> load state and document index
  -> load GGUF model
  -> open zvec collection
```

When a second OpenCode process uses the same project, it spawns another sidecar. The second sidecar attempts to acquire the same exclusive writer lock and fails with:

```text
another OpenCode process already owns this project's native memory writer lock
```

The TypeScript client treats this as a fatal handshake error. Its later requests remain failed even if the first process subsequently exits.

The lock is necessary in the current architecture because `MemoryEngine` owns mutable copies of:

- `zvec::Collection`;
- `MemoryState`;
- `DocumentIndexManifest`;
- mutable llama.cpp embedding state;
- pending journal state.

Two independent engines would not only contend on the lock. They could also hold stale lifecycle state and overwrite each other's atomic state saves. The fix must establish one authoritative owner per project, not merely weaken the file lock.

## Target Architecture

```text
OpenCode process A                         OpenCode process B
       |                                          |
       | TypeScript plugin                       | TypeScript plugin
       |                                          |
       +------------------+  +------------------+
                          |  |
                          v  v
                 Unix domain socket
                          |
                          v
                 User-level Rust daemon
                          |
              +-----------+-----------+
              |                       |
       ProjectActor A          ProjectActor B
              |                       |
       MemoryEngine A          MemoryEngine B
              |                       |
       zvec/state/model        zvec/state/model
```

For two OpenCode processes using the same project:

```text
OpenCode A --+
             +--> one daemon connection set --> one ProjectActor --> one MemoryEngine --> one writer.lock
OpenCode B --+
```

The lock remains held by the daemon-owned `MemoryEngine`, but no second normal client attempts to open the same store.

## Transport Decision

### Direct IPC

The plugin connects directly to the daemon. There is no MCP process between them:

```text
OpenCode plugin
  -> selected Protobuf IPC over Unix domain socket (gRPC preferred)
  -> Rust daemon
```

This keeps the critical path small and avoids translating internal memory operations into MCP tool calls and then back into daemon requests.

### Protobuf and gRPC roles

Use Protobuf for the canonical internal contract. Prefer gRPC for the daemon RPC transport because a shared daemon needs explicit connection, lease, deadline, status, and cancellation semantics. gRPC provides generated client/server stubs, request deadlines, metadata, status codes, and multiplexed connections. The existing length-delimited Protobuf protocol remains a viable direct-IPC fallback if the Phase 0 spike finds a packaging or runtime blocker.

The first implementation should use Unix domain sockets rather than TCP:

- Linux: `$XDG_RUNTIME_DIR` when available, otherwise a private per-user runtime directory.
- macOS: a private per-user runtime directory under the system temporary directory or the platform application-support runtime location.
- Windows later: named pipe with an owner-only ACL.

The endpoint path must be short enough for Unix socket path limits. The daemon should create an owner-only directory and a socket with mode `0600` where the platform supports those permissions.

### Phase 0 fallback

Before committing to gRPC, build a small Rust `tonic` plus TypeScript `@grpc/grpc-js` Unix-socket spike. The spike must prove:

- macOS Apple Silicon works with the packaged binary;
- Linux glibc works with the packaged binary;
- the exact grpc-js Unix target/authority syntax never falls back to TCP or a proxy;
- tonic serves the packaged socket through its Unix-listener incoming stream;
- generated Rust and TypeScript bindings agree on 64-bit IDs and Protobuf `oneof` values;
- multiple clients can use one socket concurrently;
- a disconnected client does not terminate the daemon;
- session stream half-close, channel close, backpressure, and daemon replacement are observable in TypeScript;
- message limits and macOS/Linux socket-path limits are enforced;
- connection deadlines and status errors are observable in TypeScript;
- the packaged client runs under every supported OpenCode CLI/Desktop host runtime, not only a standalone Node script.

If the spike fails because of platform or runtime constraints, retain the existing Protobuf messages and implement a small, versioned, length-delimited Protobuf RPC over the Unix socket instead. This fallback is still direct IPC, but it must not be called gRPC.

The rest of this plan uses gRPC names for the preferred branch. The fallback must implement the same transport-neutral semantics: session negotiation, heartbeat/TTL, project leases, per-call IDs and deadlines, bounded queues, in-band errors, reconnect behavior, and no automatic replay of ambiguous mutations. Shared acceptance criteria refer to the selected direct IPC transport.

### MCP boundary

MCP is deliberately not used for plugin-to-daemon IPC. MCP messages are JSON-RPC messages and standard MCP transports are stdio and Streamable HTTP. Protobuf/gRPC is not an official MCP wire transport.

If a future release needs to expose memory to an independent MCP client, add a separate MCP adapter:

```text
MCP client
  -> MCP JSON-RPC over stdio or Streamable HTTP
  -> MCP adapter
  -> private selected Protobuf IPC
  -> memory daemon
```

That adapter must be an optional boundary feature, not a dependency of the OpenCode plugin's direct tools. It should be planned separately after the daemon protocol is stable.

## Daemon Responsibilities

The daemon has four major responsibilities.

### 1. Process bootstrap

- Resolve a stable per-user endpoint.
- Ensure only one daemon instance is started per user and daemon protocol generation.
- Recover from a stale socket without deleting a live endpoint.
- Report an actionable error for an incompatible daemon already running.
- Keep logs on stderr or a daemon log sink, never on the binary protocol channel.

### 2. Connection management

- Accept multiple plugin connections.
- Assign a session ID.
- Track project leases by session.
- Use a long-lived gRPC session stream only for negotiation, heartbeat, and lease-lifecycle events.
- Use unary project RPCs so each operation retains its own gRPC deadline, cancellation signal, and status.
- Release all leases owned by a session when its stream closes.
- Add heartbeat and lease TTL cleanup as a fallback for abrupt process termination.
- Never allow one client's `dispose` or timeout to kill the daemon.

### 3. Project registry

- Canonicalize worktree and data paths.
- Derive a `StoreKey` solely from the canonical physical project data directory.
- Deduplicate concurrent opens by `StoreKey` before validating configuration.
- Compare a separate project/model/schema fingerprint after lookup and reject a mismatch without creating a second actor.
- Return the same project handle to multiple clients.
- Reject an incompatible embedding configuration for an existing collection.
- Keep one `MemoryEngine` per project actor.

### 4. Project execution

- Execute all engine operations through the owning project actor.
- Preserve current journal ordering and recovery behavior.
- Bound per-project queues.
- Keep long-running synchronous llama.cpp, zvec, filesystem, and xberg work off the async acceptor thread.
- Return typed, actionable errors without leaking sensitive memory content.

## Rust Design

### Proposed module layout

The exact names can change during implementation, but the ownership boundaries should be explicit:

```text
src/
  daemon/
    mod.rs
    endpoint.rs       # endpoint and runtime directory resolution
    bootstrap.rs      # single-instance startup and stale socket handling
    registry.rs       # project identity and actor registry
    actor.rs          # one MemoryEngine owner and serialized queue
    connection.rs     # connection and lease cleanup
    service.rs        # selected IPC service implementation
  rpc.rs              # compatibility adapter or shared request dispatch
  engine/mod.rs       # existing project engine
  embedding.rs        # existing per-engine embedder
  storage/zvec.rs     # writer lock retained as defense in depth
```

The initial implementation may keep fewer files, but it must not put daemon lifecycle state inside `MemoryEngine`.

### Project actor model

Prefer one dedicated actor thread per active project over an unrestricted `Arc<Mutex<MemoryEngine>>`.

```text
IPC request task
  -> bounded ProjectCommand queue
  -> ProjectActor thread
  -> &mut MemoryEngine
  -> response channel
```

Reasons:

- Current engine methods are synchronous.
- The embedder is mutable.
- `search` can mutate retrieval feedback state.
- zvec is a C-backed dependency whose cross-thread behavior should not be assumed.
- compound operations need one ordered transaction boundary.
- a blocking engine call must not block the async socket acceptor.

The actor should process one command at a time. Different project actors may run concurrently, subject to configured CPU, memory, and GPU limits.

### Async and multi-threaded execution

Use concurrency aggressively at boundaries that do not violate project-store ownership:

```text
Tokio multi-thread runtime
  - Unix socket accept and gRPC I/O
  - session streams, heartbeats, deadlines, timers
  - request decoding, routing, and response delivery
  - daemon health and actor-registry control plane

Thread-affine project workers
  - one active MemoryEngine owner per StoreKey
  - synchronous llama.cpp, zvec, and state transactions
  - strict one-command-at-a-time transaction lane per store

Bounded preparation pools
  - document discovery and metadata reads
  - hashing, parsing, extraction, and chunk preparation
  - work that does not touch mutable MemoryEngine state
```

Rules:

- Never run blocking llama.cpp, zvec, filesystem, or xberg work on a Tokio reactor/core thread.
- Never hold an async mutex guard across blocking work or `.await`.
- Different `StoreKey` actors may execute simultaneously.
- Multiple clients for one `StoreKey` may enqueue concurrently, but engine transactions remain serialized.
- Daemon health, session heartbeats, and lease expiry remain responsive even while a project actor is busy.
- Use bounded channels and semaphores; never create an unbounded task or OS-thread count from client traffic.
- Keep actor workers thread-affine until zvec, llama.cpp context, and `MemoryEngine` `Send`/`Sync` behavior is proven and tested.

For document ingestion and indexing, split work into prepare and commit stages:

```text
async/control plane
  -> bounded discovery/extraction/hash pool
  -> immutable prepared batch + source hash
  -> project actor revalidates source hash
  -> embed/journal/zvec/state commit
```

This allows CPU and I/O preparation to overlap across files and projects without allowing concurrent state commits for one store. If a source changes between preparation and commit, discard or re-prepare that batch.

### Resource governor

A daemon-wide resource governor prevents concurrency from becoming oversubscription:

- global cap for active project actors;
- global and per-project caps for queued commands and preparation tasks;
- CPU semaphore for embedding and extraction;
- GPU/Metal semaphore or weighted permit based on measured backend behavior;
- model-load semaphore to avoid concurrent multi-gigabyte loads exhausting memory;
- configurable per-operation thread budget;
- admission rejection with retry-after metadata when capacity is exhausted.

The current embedder defaults each model context to all available CPU threads. That policy is unsafe when several project actors run concurrently. In daemon mode, compute a daemon-wide CPU budget and allocate per-operation thread counts so total runnable embedding threads stay bounded. `OPENCODE_MEMORY_EMBEDDING_THREADS` becomes a hard per-inference maximum, not permission for every active project to consume all cores simultaneously.

Do not parallelize reads inside one store merely because a method takes `&self`. `search` mutates feedback state, the collection is C-backed, and read/write consistency has not been proven. Add parallel same-store reads only after dependency guarantees, stress tests, and benchmarks demonstrate safety and benefit.

### Project identity

Do not use the worktree string or the embedding configuration as the actor registry key. Split identity into two values:

```text
StoreKey
  = canonical physical project_data_dir

ConfigurationFingerprint
  = canonical project root
  + project ID
  + embedding model and preprocessing identity
  + memory schema compatibility
```

The `StoreKey` is the only actor-map key. The configuration fingerprint is metadata validated against the opening request, the existing actor, and the collection manifest. This guarantees that two configurations pointing at one physical store can never create two actors and contend on `writer.lock`.

The configuration fingerprint contains:

- canonical project root;
- project ID;
- embedding model identity;
- embedding dimension and relevant preprocessing configuration;
- memory schema compatibility.

Relative paths, symlinks, and alternate `OPENCODE_MEMORY_DATA_DIR` spellings must resolve to one `StoreKey` when they point to the same store. Protocol generation is negotiated per daemon session and must not split store ownership.

The daemon should return an opaque `project_handle` after `AcquireProject`. The handle is scoped to one daemon instance and session; clients must not persist it or construct it from paths.

### Project actor lifecycle

Each registry entry has an explicit state:

```text
Opening -> Ready -> Draining -> Closing
    |         |
    +------> Failed
```

- `Opening` is installed before `MemoryEngine::open()` so concurrent acquisitions await one result.
- `Ready` accepts bounded commands and leases.
- `Draining` rejects new leases while finishing admitted commands.
- `Closing` drops `MemoryEngine`, releases `writer.lock`, joins the actor thread, and removes the registry entry.
- `Failed` stores a short diagnostic/backoff result, then is removed so a later acquisition can retry.

When lease count, admitted command count, and daemon-owned job count all reach zero, start a project idle timer. On expiry, transition the actor through `Draining` and `Closing`. This prevents the daemon from retaining every model, thread, collection, and lock it has ever opened.

An actor panic must be contained, joined, and removed from the registry. The daemon must not hand out a handle to a failed actor.

### Lock behavior

Keep `src/storage/zvec.rs::acquire_writer_lock` unchanged initially except for diagnostics. The daemon-owned engine holds the lock for the actor lifetime.

The normal sequence becomes:

```text
client connects
  -> AcquireProject
  -> registry finds or creates actor
  -> only the actor calls MemoryEngine::open()
  -> writer.lock is acquired once
  -> later clients join the actor
```

The file lock still protects against:

- an old sidecar binary;
- a second daemon instance;
- a manually launched native binary;
- a crash/restart race;
- another application opening the same store.

The error message should become daemon-neutral, for example:

```text
project store is already owned by another native memory engine
```

If useful, write non-authoritative owner diagnostics next to the lock, such as daemon PID, endpoint, and protocol version. Do not replace the OS advisory lock with a PID file. PID metadata can be stale and must never decide ownership.

### Service lifecycle

Replace the current `status`-as-handshake behavior with separate operations:

```text
GetDaemonInfo
  - protocol version
  - daemon build/version
  - capabilities
  - no project open

OpenSession
  - negotiates protocol and capabilities
  - creates one heartbeat stream per plugin process
  - returns daemon instance ID and session ID

AcquireProject
  - project configuration
  - canonical project identity
  - returns project handle and lease

ProjectCall
  - one unary RPC per memory operation
  - actual engine/model/zvec/state status remains METHOD_STATUS

ReleaseProject
  - releases this client's lease only
```

The service binds leases to a client session rather than relying only on a unary `ReleaseProject` call. The preferred shape is one long-lived stream for session lifecycle plus unary RPCs for project operations:

```proto
service MemoryDaemon {
  rpc GetDaemonInfo(GetDaemonInfoRequest) returns (GetDaemonInfoResponse);
  rpc OpenSession(stream SessionControl) returns (stream SessionEvent);
  rpc AcquireProject(AcquireProjectRequest) returns (AcquireProjectResponse);
  rpc ProjectCall(ProjectCallRequest) returns (ProjectCallResponse);
  rpc ReleaseProject(ReleaseProjectRequest) returns (ReleaseProjectResponse);
}
```

The first `SessionControl` message must be `SessionHello`. `SessionReady` returns the daemon instance ID, session ID, selected protocol generation, supported domain schema, capabilities, heartbeat interval, and lease TTL. Project RPCs are rejected until that negotiation succeeds.

`OpenSession` lets the daemon release all leases when the stream closes. Heartbeat and lease TTL remain required for abrupt process termination and delayed transport cleanup. Reconnection creates a new session and reacquires project leases; it never resumes an old session or silently replays project calls.

gRPC non-OK statuses on `OpenSession` are reserved for session-fatal failures. Each unary project call has its own deadline, cancellation signal, and gRPC status, so one invalid or cancelled operation does not close the session or release unrelated leases.

Remove `METHOD_SHUTDOWN` from the normal plugin client lifecycle. A plugin must never send a global shutdown command to a shared daemon.

Daemon shutdown should be internal and occur only after:

- no active connections;
- no project leases;
- no queued or active commands;
- no daemon-owned jobs;
- idle timeout has elapsed.

A separate developer/admin command can request shutdown after authenticating the local control path.

## Protocol Plan

### Proto files

Keep the current domain values initially, but separate daemon lifecycle messages from memory operation messages:

```text
schema/opencode/memory/v1/memory.proto          # existing domain methods and values
schema/opencode/memory/daemon/v1/daemon.proto   # daemon service and session/lease protocol
```

Use package `opencode.memory.daemon.v1` for the new service and import the existing `opencode.memory.v1` domain messages. A future breaking daemon service must use `opencode.memory.daemon.v2`; do not break the `v1` service in place.

Proposed shape:

```proto
service MemoryDaemon {
  rpc GetDaemonInfo(GetDaemonInfoRequest) returns (GetDaemonInfoResponse);
  rpc OpenSession(stream SessionControl) returns (stream SessionEvent);
  rpc AcquireProject(AcquireProjectRequest) returns (AcquireProjectResponse);
  rpc ProjectCall(ProjectCallRequest) returns (ProjectCallResponse);
  rpc ReleaseProject(ReleaseProjectRequest) returns (ReleaseProjectResponse);
}

message SessionControl {
  oneof body {
    SessionHello hello = 1;
    SessionHeartbeat heartbeat = 2;
  }
}

message SessionEvent {
  oneof body {
    SessionReady ready = 1;
    DaemonDraining draining = 2;
    DaemonError error = 3;
  }
}

message SessionHello {
  string client_instance_id = 1;
  uint32 minimum_protocol_generation = 2;
  uint32 maximum_protocol_generation = 3;
  uint32 domain_schema_generation = 4;
  string plugin_version = 5;
}

message SessionReady {
  string daemon_instance_id = 1;
  string session_id = 2;
  uint32 selected_protocol_generation = 3;
  uint32 domain_schema_generation = 4;
  repeated string capabilities = 5;
  uint32 heartbeat_interval_seconds = 6;
  uint32 lease_ttl_seconds = 7;
}

message AcquireProjectRequest {
  string daemon_instance_id = 1;
  string session_id = 2;
  string project_root = 3;
  string worktree = 4;
  string data_dir = 5;
  EmbeddingIdentity embedding = 6;
}

message AcquireProjectResponse {
  string project_handle = 1;
  string lease_id = 2;
  string canonical_project_id = 3;
  string store_key_hash = 4;
}

message ProjectCallRequest {
  string daemon_instance_id = 1;
  string session_id = 2;
  string project_handle = 3;
  string lease_id = 4;
  string call_id = 5;
  int64 deadline_unix_ms = 6;
  opencode.memory.v1.Request request = 7;
}

message ProjectCallResponse {
  string call_id = 1;
  opencode.memory.v1.Response response = 2;
}
```

The exact messages should be finalized during the gRPC spike. A generic unary `ProjectCall` keeps the first migration small and reuses the existing `Method`/`Value` dispatch. A later daemon `v2` can replace it with typed RPC methods if the generic envelope becomes a limitation.

### Request semantics

- Every logical request has an opaque `call_id`; transport request IDs must not be reused as durable operation identity.
- Every engine call has an application deadline carried in the request envelope; the gRPC session stream itself must not be the only deadline boundary.
- `AcquireProject` is idempotent per session, `StoreKey`, and configuration fingerprint.
- `ReleaseProject` is idempotent.
- Closing the session stream releases all its leases; heartbeat expiry provides crash cleanup.
- A deadline is enforced before queue admission, while queued, and before the engine transaction begins.
- A deadline or disconnect does not cancel a running non-cancellable engine operation unless cancellation is explicitly supported for that operation.
- A response for a disconnected client may be discarded after the engine reaches a transaction boundary.
- Unknown methods and incompatible protocol versions fail explicitly.

### Retry and mutation outcomes

The client must never automatically replay an admitted mutation after a transport failure. A journal can recover storage consistency, but it cannot by itself tell the client whether a lost response was committed.

Classify operations as follows:

- Retry-safe reads: `get`, `list`, `status`, `doctor`, and `export`, when they do not create retrieval state.
- Conditionally retry-safe operations: `search` only when feedback tracking is disabled and the response semantics are explicitly declared read-only.
- Non-retry-safe operations: `store`, `capture`, `update`, `pin`, `lock`, `delete`, `forget`, `purge`, `feedback`, `sync_shared`, `import`, `ingest`, `index_documents`, and `optimize`.

If a non-retry-safe call may have reached the actor and its response is lost, return `OUTCOME_UNKNOWN` with its `call_id`. Do not silently replay it. A later operation-outcome query and durable receipts/idempotency keys are required before automatic mutation replay can be enabled. Durable receipts belong in the daemon/job follow-up phase and must be designed with the current state schema instead of being added as an unversioned side file.

### Protocol versioning

Use separate version fields for:

- daemon RPC protocol;
- domain/memory schema;
- embedding model identity and dimension;
- plugin/daemon build compatibility.

`SessionHello` must provide the client's minimum and maximum supported daemon generations, domain schema generation, plugin version, and client instance ID. `SessionReady` returns the selected generation, domain schema, daemon instance ID, capabilities, heartbeat interval, and lease TTL. Every project handle and lease is scoped to the returned daemon instance ID and session ID.

Do not use the existing `RPC_PROTOCOL_VERSION = 2` as the only compatibility signal after daemonization. Add a daemon protocol generation and a build fingerprint. A build fingerprint is diagnostic; compatibility is determined by the negotiated generation and capability set.

The daemon endpoint is stable and unversioned for a user. An incompatible client must not start a second daemon against the same stores. It must wait for or request a controlled drain of the existing daemon according to the upgrade state machine below.

Protobuf compatibility rules:

- Never reuse field numbers.
- Reserve removed enum values and fields.
- Add fields compatibly before removing old ones.
- Pin the Rust and TypeScript code generators.
- Run `buf lint` and `buf breaking` in CI.
- Add a cross-language golden-frame test for important messages.

### Code generation

Current generated TypeScript uses `@bufbuild/protobuf` and `protoc-gen-es`. The daemon service needs a gRPC-compatible TypeScript generator and Rust server bindings. The generator choice must be made in Phase 0 and pinned in `package.json`, `Cargo.toml`, and Buf configuration.

### Numeric representation

The current schema contains `uint64`/`sint64`, while the current TypeScript client uses JavaScript safe integers for request IDs. Freeze an explicit policy in Phase 0:

- new daemon/session/lease/call identifiers use opaque strings or bytes, not JavaScript numbers;
- generated TypeScript uses `bigint` for wire-level 64-bit fields where the domain contract requires full range;
- conversion to the existing JSON-facing memory contract is allowed only for values within `Number.MAX_SAFE_INTEGER`;
- values above the safe range are returned as decimal strings or rejected with a typed conversion error;
- golden tests cover zero, `2^53 - 1`, values above `2^53`, explicit null versus absent `oneof`, unknown fields, and unknown enum values.

Do not rely on the generator's default `Long`, `number`, or string behavior without testing the exact runtime used by the packaged OpenCode plugin.

The migration must update:

- `schema/opencode/memory/v1/memory.proto`;
- `schema/opencode/memory/daemon/v1/daemon.proto`;
- `buf.yaml` and any `buf.gen.yaml` added by the spike;
- `build.rs` and Rust generated service output;
- committed TypeScript generated files;
- protocol/client tests.

## TypeScript Plugin Changes

### Replace sidecar ownership with daemon connection ownership

Refactor `opencode-memory/src/sidecar-client.ts` into a daemon client, or introduce `daemon-client.ts` behind a stable `NativeMemoryClient`-like interface.

The plugin-facing interface should continue to expose:

```ts
request<T>(method, params, signal?): Promise<T>
dispose(): Promise<void>
```

This lets `plugin.ts` and existing tool implementations migrate without changing every memory tool at once.

Internally, the client must:

- connect to the user daemon;
- perform `GetDaemonInfo`;
- open and maintain one negotiated session stream per daemon channel;
- acquire a project lease scoped to that session;
- route calls with project handle and lease ID;
- reconnect after a daemon restart by creating a new session and reacquiring leases;
- never replay an admitted non-retry-safe call after reconnect;
- surface `OUTCOME_UNKNOWN` for an ambiguous mutation result;
- release only its own lease on dispose;
- avoid killing a process it does not own.

The in-process pool should use one daemon channel/session per endpoint and local reference-counted project subleases. Multiple plugin instances in the same JavaScript process may share that session. Releasing the last local reference for one project sends `ReleaseProject`; closing the session stream and channel happens only when no local project entries remain.

### Bootstrap algorithm

Use this sequence:

```text
1. Resolve the canonical daemon endpoint.
2. Try connecting to the endpoint.
3. If connected, negotiate protocol and use it.
4. If connection fails because endpoint is absent, acquire daemon-start lock.
5. Re-check the endpoint after acquiring the lock.
6. If still absent, spawn the packaged daemon in --daemon mode.
7. Wait for readiness with bounded backoff.
8. Release the daemon-start lock.
9. Connect, negotiate, open a session, and acquire the project lease.
```

The bootstrap lock is a user-level daemon-start lock, not the project `writer.lock`. It exists only to prevent several OpenCode processes from spawning several daemons at the same time.

Use two owner-only locks in the fixed runtime directory:

- `daemon-start.lock`: held briefly by a plugin bootstrapper while it re-checks the endpoint, starts a daemon, and waits for readiness;
- `daemon-lifetime.lock`: held by the daemon for its entire lifetime, including endpoint probe, stale-socket cleanup, bind, and readiness publication.

Every daemon entry path must acquire `daemon-lifetime.lock` before touching the endpoint. This prevents two compliant daemon generations from probing or binding concurrently. The plugin must never unlink a socket by itself. A daemon may unlink a socket only after checking that it is a dead endpoint owned by the current user; an incompatible live daemon must not be deleted.

The client must distinguish:

- endpoint absent;
- endpoint alive but incompatible;
- endpoint alive but owned by a different OS user;
- daemon starting;
- project store owned by a legacy sidecar;
- project configuration mismatch.

Every error must state the next action, such as restarting the daemon, closing a legacy OpenCode process, or rebuilding the native package.

### Pool behavior

Keep the existing in-process pool for duplicate plugin instances, but make it a pool of daemon connections and leases. Cross-process sharing is provided by the daemon, not by `globalThis`.

One pool entry owns one daemon channel and session. It may contain multiple local project subleases. A local plugin instance increments the relevant sublease reference count. The last local reference for a project sends `ReleaseProject`; the last project entry closes the session stream and channel. A plugin dispose must not close a channel that another local plugin instance still uses.

The pool key should use the canonical project-store identity returned or validated by the daemon. Do not rely only on the current string key:

```text
canonical worktree + raw OPENCODE_MEMORY_DATA_DIR
```

### Timeout and failure behavior

The current behavior of killing the child process on timeout cannot be reused.

New behavior:

- queued request timeout: remove it from the client queue when safe;
- in-flight retry-safe read timeout: reject locally and keep the daemon connection alive;
- in-flight non-retry-safe timeout: reject with a definite or `OUTCOME_UNKNOWN` result; never replay automatically;
- connection failure: reconnect with bounded retry, create a new session, and reacquire the project lease;
- daemon crash during a mutation: reconnect and let `MemoryEngine::open()` run journal recovery;
- repeated daemon crashes: stop retrying and return an actionable error;
- daemon shutdown: only the daemon's idle policy can terminate it.

### Plugin lifecycle

`createMemoryPlugin` continues to acquire a lease during plugin initialization. The key change is that this is a daemon lease, not ownership of a child process.

On disposal:

1. Stop accepting new client-local requests.
2. Drain or cancel client-local work that has not been sent.
3. Release the project lease.
4. Close the gRPC channel only when this process has no remaining project subleases.
5. Do not call `shutdown`.

## Background Jobs and Indexing

The current `BackgroundJobQueue` is local to each plugin process. Consequently, `memory_ingest_status` is invisible to other OpenCode processes.

### First migration

Keep the existing queue temporarily, but ensure all Rust operations go through the shared project actor. This is the smallest safe change and fixes concurrent zvec/state/model ownership.

Document the temporary behavior clearly:

- ingestion job IDs are client-local;
- a job may disappear when its OpenCode process exits;
- the daemon still serializes the actual engine request.

### Follow-up migration

Move durable jobs into the daemon:

- daemon-owned job ID;
- queued/running/succeeded/failed state;
- bounded queue capacity;
- status visible to all clients;
- cancellation only before execution starts;
- durable recovery for jobs interrupted during an engine transaction;
- coalescing of duplicate `index_documents` requests from multiple clients.

The current `index_documents` call is one monolithic synchronous engine operation. Queue priority cannot preempt it after execution begins. Refactor indexing into bounded batches with actor yield points and deadline/cancellation checks between batches while preserving valid journal and manifest boundaries. Daemon-level `GetDaemonInfo` remains responsive independently of a busy project actor; project calls receive fairness only at defined batch boundaries.

This should happen before enabling aggressive automatic indexing across many OpenCode processes.

## Embedding Model Ownership

### First release guarantee

For one project:

```text
N OpenCode processes
  -> 1 daemon
  -> 1 ProjectActor
  -> 1 MemoryEngine
  -> 1 LlamaCppEmbedder/model context
```

This is the required fix for duplicate same-project loading and `.lock` failures.

For different projects in the same daemon, the first release may have one embedder per active project actor:

```text
Project A -> MemoryEngine A -> model context A
Project B -> MemoryEngine B -> model context B
```

The model file remains in the shared revisioned cache, so it is downloaded once. Runtime model/context memory is not necessarily shared.

### Future model registry

Do not refactor model ownership before the project actor migration is stable. If memory measurements justify it, introduce a model registry keyed by full embedding identity:

- repository and revision;
- file path or downloaded artifact hash;
- pooling and attention;
- templates and BOS/EOS behavior;
- dimension;
- context size;
- GPU layers and backend.

Sharing only immutable model weights while keeping a serialized context per project may reduce memory without exposing the mutable llama context concurrently. This is a separate optimization and requires benchmark and GPU validation.

## Security and Reliability

### Local IPC

- Bind only to a Unix domain socket or equivalent local-only primitive.
- Place the endpoint in an owner-only directory.
- Set socket permissions to owner-only.
- Verify endpoint ownership and liveness before removing a stale socket; only the daemon holding `daemon-lifetime.lock` may remove a dead socket.
- Version 1 trusts processes running as the same OS user. Enforce owner-only directory/socket permissions and validate peer UID where the platform exposes it.
- Do not use a TCP listener, even on loopback, for the local daemon.
- Do not bind to `0.0.0.0` or expose the daemon remotely.
- Keep daemon logs off the protocol stream.

### Process integrity

- Resolve the daemon from the packaged native binary path.
- Do not execute a workspace-controlled binary unless explicitly enabled for development.
- Pass fixed argument arrays; do not use a shell.
- Sanitize inherited environment values.
- Keep the daemon at normal user privileges.
- Include daemon build/version in handshake diagnostics.

### Resource limits

- Limit message size before decoding and allocation.
- Bound connection count and per-project queue size.
- Bound total active project actors.
- Bound outstanding unary calls per session.
- Add configurable idle timeout.
- Apply per-call deadlines to unary operations and heartbeat/TTL rules to the long-lived session stream.
- Reject expired work before actor admission and again before execution.
- Implement these baseline limits before the TypeScript plugin switches to daemon traffic.
- Split long indexing into batches before promising interactive fairness.
- Add metrics for model load time, queue wait, operation latency, and actor count.

### Crash recovery

The existing journal and `MemoryEngine::open()` recovery are valuable. Preserve their ordering. Add tests that terminate the daemon during:

- pending upsert journal write;
- zvec upsert;
- final state save;
- pending delete journal write;
- zvec delete;
- document-index manifest update.

After restart, the daemon must open one actor and recover before serving the next project operation.

## Packaging and Release

### Native package

The platform native packages currently contain the `opencode-memory` executable and zvec shared libraries. Add daemon mode to the same binary first:

```text
opencode-memory --daemon --endpoint <resolved-endpoint>
```

This avoids publishing a second native package during the first migration. A later release may split a daemon package if lifecycle or platform constraints require it.

### TypeScript package

The package must ship a Node-compatible JavaScript client/runtime. Do not depend on OpenCode being able to execute TypeScript from `node_modules`; this is a known issue in the Desktop runtime ecosystem.

Add the gRPC runtime and generated client dependencies only after the Phase 0 spike confirms the package size and platform behavior. Pin versions and include them in package verification.

### Version compatibility

The daemon can outlive the plugin process that started it. Therefore:

- plugin and daemon negotiate compatibility;
- an incompatible daemon is not silently killed by a new plugin;
- the client reports the daemon PID, endpoint, version, and required action;
- idle timeout limits how long old daemons remain around;
- one stable endpoint and one `daemon-lifetime.lock` are shared by all daemon generations;
- a controlled upgrade path can request drain and restart only when no active clients, leases, jobs, or admitted commands remain.

The upgrade state machine is:

```text
Running
  -> DrainRequested
  -> Draining (reject new sessions and project leases)
  -> Stopped (close actors, release writer locks, remove endpoint)
  -> Starting (new daemon acquires daemon-lifetime.lock and binds endpoint)
  -> Running
```

An incompatible client must not create a generation-specific endpoint. If the current daemon is busy, the client returns its version, PID, and a clear restart action. If the daemon is idle and the local control policy permits it, the client can request drain; otherwise the user restarts after active clients exit. The daemon itself, not a plugin, owns endpoint removal and lifetime-lock release.

During the transition, keep a temporary environment flag:

```text
OPENCODE_MEMORY_TRANSPORT=sidecar|daemon
```

Use `sidecar` only as a rollback path during beta testing. Remove the flag after the daemon path is proven in release CI unless a permanent compatibility mode is explicitly required.

## Rollout Phases

### Phase 0: Architecture spike

Deliverables:

- gRPC over Unix domain socket proof of concept;
- generated Rust and TypeScript service bindings;
- endpoint resolver for macOS and Linux;
- two independent clients connecting to one daemon;
- session negotiation, heartbeat, lease TTL, and unary per-call deadlines;
- exact packaged-runtime UDS startup/reconnect demonstration;
- decision record for gRPC versus framed Protobuf fallback.

Exit criteria:

- no TCP listener;
- no duplicate daemon under a concurrent startup test;
- one client disconnect does not terminate the other client's request;
- a dead socket cannot be unlinked by a client or while a compliant daemon owns the lifetime lock;
- all supported OpenCode host runtimes can use the packaged generated client;
- package build works on supported native targets.

### Phase 1: Daemon shell and project registry

Deliverables:

- `--daemon` mode;
- stable user-level endpoint, daemon-start lock, and cross-generation daemon-lifetime lock;
- daemon acceptor and connection cleanup;
- session handshake and lease TTL cleanup;
- project registry keyed by canonical `StoreKey` with separate configuration fingerprint validation;
- `ProjectActor` owning one `MemoryEngine`;
- `AcquireProject`, `ProjectCall`, and `ReleaseProject` unary RPCs;
- bounded message/session/actor/queue limits;
- actor open/ready/draining/closing/failed state machine;
- old `writer.lock` retained.

Exit criteria:

- two OpenCode-like clients acquire the same project successfully;
- `MemoryEngine::open()` runs once for that project;
- two configurations targeting one `StoreKey` produce a configuration mismatch without creating a second actor;
- one `writer.lock` remains held by the daemon;
- zero-lease actor eviction deterministically releases that lock and model;
- `status`, `store`, `search`, `get`, and `list` work from both clients.

### Phase 2: TypeScript daemon client

Deliverables:

- daemon client abstraction behind the current plugin request interface;
- bootstrap/reconnect logic;
- cross-process-safe disposal;
- timeout behavior that does not kill the daemon;
- session reconnect and lease reacquisition without replaying admitted mutations;
- protocol-version conflict detection and controlled drain request;
- temporary sidecar fallback flag;
- plugin integration in `createMemoryPlugin`.

Exit criteria:

- existing plugin tools pass without API changes;
- disposing OpenCode process A leaves process B functional;
- daemon restart causes bounded reconnect and journal recovery;
- ambiguous mutations are not replayed and return `OUTCOME_UNKNOWN`;
- lock-conflict fatal state is no longer produced for normal same-project clients.

### Phase 3: Concurrency, jobs, and observability

Deliverables:

- project queue limits and fairness;
- daemon metrics and structured diagnostics;
- daemon-owned ingestion jobs and status;
- duplicate index request coalescing;
- cancellation semantics for queued work;
- bounded/batched document indexing with actor yields between batches;
- mutation outcome receipts or an explicit outcome-query API before any automatic replay is considered;
- crash recovery integration tests.

Exit criteria:

- simultaneous interactive requests do not corrupt state;
- daemon health remains responsive while a project actor is busy;
- batched indexing gives interactive project calls defined fairness points;
- job state is visible across OpenCode processes;
- queue overflow returns an actionable error instead of spawning more engines.

### Phase 4: Packaging and release migration

Deliverables:

- native package daemon startup verification;
- macOS and Linux release tests;
- protocol generation checks;
- daemon version conflict handling;
- upgrade/drain behavior;
- documentation and environment variable updates;
- release notes and rollback instructions.

Exit criteria:

- clean install starts the daemon on first memory operation;
- multiple OpenCode processes share it;
- an old daemon cannot be used silently with an incompatible client;
- package verification covers binary, shared libraries, generated bindings, and daemon startup.

### Phase 5: Optional MCP adapter, only if required

Do not block the daemon migration on MCP. If product requirements later call for MCP interoperability:

- implement MCP JSON-RPC over stdio or Streamable HTTP;
- map MCP tools to the stable daemon IPC API;
- keep the adapter in a separate process or explicit binary mode;
- add JSON Schema and error translation tests;
- optionally let the OpenCode plugin add the MCP entry through its in-memory `config` hook, following the `@sveltejs/opencode` pattern;
- never route the direct OpenCode plugin tools through that MCP adapter.

The plugin should not run a `postinstall` script to mutate user files. OpenCode installs the npm plugin package; the plugin's `config` hook may compose the in-memory config for the current process. Persistent config scaffolding, if ever added, should be an explicit command.

## Test Plan

### Unit tests

- canonical `StoreKey` path aliases and separate configuration fingerprints;
- daemon endpoint resolution and socket path length;
- stale socket detection;
- daemon-start and daemon-lifetime lock behavior;
- lease idempotency;
- session hello/ready state machine, heartbeat expiry, and connection cleanup;
- protocol version negotiation;
- Protobuf 64-bit IDs and `oneof` values;
- queue capacity and ordering;
- deadline-before-admission and deadline-before-execution behavior;
- retry-safe versus non-retry-safe disconnect behavior;
- daemon version mismatch.

### Rust integration tests

- two clients acquire the same project;
- one project actor opens the engine once;
- same store with a different model fingerprint is rejected before another engine open;
- second client joins without a second `MemoryEngine` or model load;
- both clients execute reads and writes safely;
- client A disconnects while client B continues;
- actor eviction drops the engine and allows a clean later reopen;
- daemon exits idle only after all work completes;
- lock remains held while actor exists;
- legacy sidecar receives an actionable conflict error;
- a lost mutation response returns `OUTCOME_UNKNOWN` and is not replayed;
- journal recovery after daemon termination at each transaction boundary.

### TypeScript integration tests

- concurrent bootstrap starts exactly one daemon;
- incompatible generations use one stable endpoint and never run two daemons concurrently;
- two plugin instances in separate OS processes share one endpoint;
- `dispose()` releases a lease but does not send global shutdown;
- request timeout does not kill another client's operation;
- reconnect creates a new session, reacquires the project lease, and does not resume old calls;
- plugin request API remains compatible with current tools;
- no MCP process is spawned on the direct plugin path.

### Black-box packaging tests

- install each supported optional native package;
- start the packaged daemon from the installed path;
- connect through the packaged TypeScript client;
- verify zvec shared-library resolution after detaching from the plugin cwd;
- run two independent OpenCode-like processes against one temporary project;
- verify only one model/engine open through daemon diagnostics;
- verify daemon idle shutdown and restart.

### Performance tests

Measure before and after:

- first model load latency;
- warm status latency;
- p50/p95 search latency with one client;
- p50/p95 search latency with two and four clients;
- queue wait under document indexing;
- resident memory for one project and multiple projects;
- GPU/unified-memory usage where Metal is enabled;
- daemon startup and reconnect latency.

## Acceptance Criteria

The migration is complete when all of the following are true:

1. Two or more OpenCode processes can use the same project concurrently.
2. Only one daemon-owned `MemoryEngine` opens that project store.
3. The normal same-project path produces no writer-lock handshake failure.
4. The daemon retains one writer lock as defense in depth.
5. One client can dispose or time out without killing the daemon or disrupting other clients.
6. State, zvec, document index, and embedding operations remain serialized per project.
7. Daemon crash recovery preserves current journal guarantees.
8. Direct plugin tools use IPC without MCP.
9. The selected Protobuf IPC protocol and generated bindings are versioned and checked in CI.
10. Supported native packages can bootstrap and serve the daemon on macOS and Linux.
11. MCP remains an optional future adapter and is not required for installation or normal memory operation.
12. A transport failure never automatically replays a mutation with an ambiguous outcome.
13. Actor eviction and daemon drain release models, threads, collections, sockets, and writer locks deterministically.

## Risks and Mitigations

| Risk                                      | Impact                                           | Mitigation                                                           |
| ----------------------------------------- | ------------------------------------------------ | -------------------------------------------------------------------- |
| Multiple daemon versions coexist          | Two daemons contend for project stores           | Stable endpoint and cross-generation lifetime lock; controlled drain |
| Stale socket                              | Plugin cannot connect or deletes a live endpoint | Only lifetime-lock owner validates and removes a dead socket         |
| Actor queue starvation                    | Interactive recall is delayed by indexing        | Bounded queue, fairness/priority policy, metrics                     |
| Client timeout during mutation            | Client cannot tell whether operation committed   | No automatic replay; return `OUTCOME_UNKNOWN`; add durable receipts  |
| Daemon crash during zvec/state update     | Inconsistent storage                             | Preserve current journals and add kill-point integration tests       |
| Different config for same physical store  | Wrong model or incompatible vectors              | Actor key is `StoreKey`; validate a separate fingerprint             |
| gRPC runtime/package size                 | Larger install or platform incompatibility       | Phase 0 spike; retain framed-Protobuf fallback                       |
| Unsafe local endpoint                     | Another OS user reads/writes memory              | Owner-only runtime directory/socket and peer-UID validation          |
| Future MCP duplication                    | Duplicate tools in OpenCode                      | Keep MCP optional and separate from direct plugin tools              |
| Model memory remains high across projects | RAM/VRAM still scales with active projects       | Measure first; implement model registry only as a later optimization |

## Files and Symbols to Change

### Rust

- `src/main.rs`: add daemon mode dispatch.
- `src/rpc.rs`: split current stdio service dispatch from daemon service implementation; remove shared-daemon use of global shutdown.
- `src/engine/mod.rs`: preserve engine semantics; make ownership and open/recovery callable by one project actor.
- `src/storage/zvec.rs`: retain writer lock; improve daemon-neutral diagnostics if needed.
- `src/config.rs`: expose canonical project/data identity and daemon-safe config construction.
- `src/embedding.rs`: no first-phase model sharing refactor; add load diagnostics if needed.
- `build.rs`: add selected IPC service code generation if required by the spike.
- new `src/daemon/*`: endpoint, bootstrap, registry, actor, connection, and service boundaries.

### TypeScript

- `opencode-memory/src/sidecar-client.ts`: replace child lifecycle with daemon connection lifecycle, or move implementation to a new `daemon-client.ts`.
- `opencode-memory/src/plugin.ts`: acquire/release daemon project lease; preserve tool behavior.
- `opencode-memory/src/protocol.ts`: retain domain mapping and add daemon envelope/client bindings as appropriate.
- `opencode-memory/src/background-jobs.ts`: later move durable job state to daemon.
- `opencode-memory/src/server.ts`: keep the OpenCode plugin entrypoint stable.
- `opencode-memory/src/index.ts`: export the new client only if it is part of the supported test surface.
- `package.json`: add pinned gRPC runtime/codegen dependencies only if the spike selects gRPC.

### Schema and tests

- `schema/opencode/memory/v1/memory.proto`: preserve existing field numbers and methods.
- `schema/opencode/memory/daemon/v1/daemon.proto`: add daemon lifecycle and project lease messages.
- `buf.yaml`: add lint/breaking configuration for the new service schema.
- `opencode-memory/src/generated/*`: regenerate and check in TypeScript bindings.
- `opencode-memory/src/sidecar-client.test.ts`: migrate lifecycle tests to daemon connection semantics.
- `opencode-memory/src/protocol.test.ts`: add daemon envelopes and generated service tests.
- new Rust daemon integration tests under `tests/`.
- new multi-process black-box test scripts under `scripts/` or `tests/`.
- `.github/workflows/ci.yml`: run daemon startup, same-project concurrency, and packaging tests.

## References

### Repository evidence

- Current client pool and child process lifecycle: [`opencode-memory/src/sidecar-client.ts`](../opencode-memory/src/sidecar-client.ts)
- Plugin lease and disposal lifecycle: [`opencode-memory/src/plugin.ts`](../opencode-memory/src/plugin.ts)
- Lazy engine initialization and status dispatch: [`src/rpc.rs`](../src/rpc.rs)
- Engine ownership and model load order: [`src/engine/mod.rs`](../src/engine/mod.rs)
- Embedding model load: [`src/embedding.rs`](../src/embedding.rs)
- Current writer lock: [`src/storage/zvec.rs`](../src/storage/zvec.rs)
- Existing Protobuf contract: [`schema/opencode/memory/v1/memory.proto`](../schema/opencode/memory/v1/memory.proto)
- Current package/build configuration: [`package.json`](../package.json)

### External protocol references

- EDB, [Building MCP Servers from Protobuf (Part 1): Protobuf to REST API](https://www.enterprisedb.com/blog/building-mcp-servers-protobuf-part1-protobuf-rest-api): useful for shared Protobuf/code-generation concepts, but its REST gateway is not MCP and is not needed for this local daemon.
- MCP, [Base Protocol](https://modelcontextprotocol.io/specification/2025-11-25/basic): MCP messages use JSON-RPC 2.0.
- MCP, [Transports](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports): standard MCP transports are stdio and Streamable HTTP.
- gRPC, [Introduction](https://grpc.io/docs/what-is-grpc/introduction/): service/stub model and Protobuf usage.
- Protocol Buffers, [Overview](https://protobuf.dev/overview/): schema and generated serialization model.
- Svelte, [OpenCode plugin](https://svelte.dev/docs/ai/opencode-plugin): reference for plugin-owned config composition, not for the direct daemon IPC path.
- Svelte `@sveltejs/opencode` implementation at commit [`c240d44`](https://github.com/sveltejs/ai-tools/blob/c240d44edae00a4fb102dfbf60593277065aa378/packages/opencode/index.js): detects/injects MCP in OpenCode's in-memory config. This plan intentionally does not put that MCP injection in the direct daemon path.
