mod actor;
mod endpoint;
mod registry;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{Semaphore, mpsc};

use self::actor::{CALL_CANCELLED, CALL_COMPLETED, CALL_QUEUED, CALL_RUNNING, ProjectActor};
use self::endpoint::EndpointGuard;
use self::registry::ProjectRegistry;
use crate::daemon_proto::daemon_request;
use crate::daemon_proto::daemon_response;
use crate::daemon_proto::{
    AcquireProjectRequest, AcquireProjectResponse, CancelCallRequest, CancelCallResponse,
    CancelOutcome, DaemonRequest, DaemonResponse, DaemonStatus, DaemonStatusCode, DrainOutcome,
    GetDaemonInfoResponse, OpenSessionRequest, OpenSessionResponse, ProjectCallRequest,
    ProjectCallResponse, ReleaseProjectResponse, RequestDrainResponse, SessionHeartbeatResponse,
};
use crate::memory_proto::Method;
use crate::rpc::{
    MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, ProjectRequest, ProjectResponse,
    validate_project_request,
};
use crate::{EmbeddingConfig, MemoryConfig};

pub const DAEMON_PROTOCOL_GENERATION: u32 = 1;
pub const DOMAIN_SCHEMA_GENERATION: u32 = 5;
const MAX_SESSIONS: usize = 64;
const MAX_CONNECTIONS: usize = 64;
const MAX_OUTSTANDING_CALLS_PER_CONNECTION: usize = 32;
const MAX_CALL_IDS_PER_SESSION: usize = 1_024;
const MAX_OPAQUE_ID_BYTES: usize = 128;
const HEARTBEAT_INTERVAL_SECONDS: u32 = 10;
const LEASE_TTL_SECONDS: u32 = 30;
const DEFAULT_PROJECT_IDLE_SECONDS: u64 = 5 * 60;
const DEFAULT_DAEMON_IDLE_SECONDS: u64 = 10 * 60;
const DEFAULT_MAINTENANCE_INTERVAL_SECONDS: u64 = 5 * 60;
const MAX_CALL_TIMEOUT_MS: u32 = 2 * 60 * 60 * 1_000;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct ProjectLease {
    handle: String,
    lease_id: String,
    actor: Arc<ProjectActor>,
}

struct ActiveCall {
    project_handle: String,
    lease_id: String,
    state: Arc<AtomicU8>,
}

struct Session {
    connection_id: String,
    last_heartbeat: Instant,
    projects: HashMap<PathBuf, ProjectLease>,
    calls: HashMap<String, ActiveCall>,
}

struct DaemonState {
    instance_id: String,
    registry: ProjectRegistry,
    sessions: Mutex<HashMap<String, Session>>,
    active_connections: AtomicUsize,
    last_activity: Mutex<Instant>,
    drain_requested: AtomicBool,
    admission: Mutex<()>,
    drain_not_before: Mutex<Option<Instant>>,
}

impl DaemonState {
    fn new() -> Self {
        Self {
            instance_id: opaque_id("daemon"),
            registry: ProjectRegistry::new(),
            sessions: Mutex::new(HashMap::new()),
            active_connections: AtomicUsize::new(0),
            last_activity: Mutex::new(Instant::now()),
            drain_requested: AtomicBool::new(false),
            admission: Mutex::new(()),
            drain_not_before: Mutex::new(None),
        }
    }

    fn touch(&self) {
        *self
            .last_activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
    }

    fn session_count(&self) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    fn try_acquire_connection(&self) -> bool {
        self.active_connections
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < MAX_CONNECTIONS).then_some(current + 1)
            })
            .is_ok()
    }

    fn release_connection(&self) {
        self.active_connections.fetch_sub(1, Ordering::AcqRel);
    }

    fn is_busy(&self) -> bool {
        self.session_count() > 0 || self.registry.has_activity()
    }

    fn close_connection(&self, connection_id: &str) {
        let _admission = self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (closed_session, released) = {
            let mut sessions = self
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let ids = sessions
                .iter()
                .filter_map(|(id, session)| {
                    (session.connection_id == connection_id).then_some(id.clone())
                })
                .collect::<Vec<_>>();
            let closed_session = !ids.is_empty();
            let released = ids
                .into_iter()
                .filter_map(|id| sessions.remove(&id))
                .flat_map(|session| {
                    cancel_queued_calls(&session.calls);
                    session.projects.into_values()
                })
                .collect::<Vec<_>>();
            (closed_session, released)
        };
        for lease in released {
            lease.actor.release_lease();
        }
        if closed_session {
            self.touch();
        }
    }

    fn expire_sessions(&self) {
        let ttl = Duration::from_secs(u64::from(LEASE_TTL_SECONDS));
        let released = {
            let mut sessions = self
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let ids = sessions
                .iter()
                .filter_map(|(id, session)| {
                    (session.last_heartbeat.elapsed() >= ttl).then_some(id.clone())
                })
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| sessions.remove(&id))
                .flat_map(|session| {
                    cancel_queued_calls(&session.calls);
                    session.projects.into_values()
                })
                .collect::<Vec<_>>()
        };
        for lease in released {
            lease.actor.release_lease();
        }
    }
}

pub fn run(endpoint: PathBuf) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot create daemon Tokio runtime")?
        .block_on(run_async(endpoint))
}

async fn run_async(endpoint: PathBuf) -> Result<()> {
    let (listener, endpoint_guard, uid) = EndpointGuard::bind(&endpoint)?;
    let state = Arc::new(DaemonState::new());
    let project_idle = configured_duration(
        "OPENCODE_MEMORY_PROJECT_IDLE_SECONDS",
        DEFAULT_PROJECT_IDLE_SECONDS,
    );
    let daemon_idle = configured_duration(
        "OPENCODE_MEMORY_DAEMON_IDLE_SECONDS",
        DEFAULT_DAEMON_IDLE_SECONDS,
    );
    let maintenance_interval = configured_duration(
        "OPENCODE_MEMORY_MAINTENANCE_INTERVAL_SECONDS",
        DEFAULT_MAINTENANCE_INTERVAL_SECONDS,
    );
    let mut maintenance = tokio::time::interval(Duration::from_secs(1));

    loop {
        tokio::select! {
            accepted = listener.accept(), if !state.drain_requested.load(Ordering::Acquire) => {
                let (stream, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        eprintln!("native memory daemon accept failed: {error}");
                        continue;
                    }
                };
                let peer = match stream.peer_cred() {
                    Ok(peer) => peer,
                    Err(error) => {
                        eprintln!("native memory daemon peer credential check failed: {error}");
                        continue;
                    }
                };
                if peer.uid() != uid {
                    continue;
                }
                if !state.try_acquire_connection() {
                    continue;
                }
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    if let Err(error) = serve_connection(stream, Arc::clone(&state)).await {
                        eprintln!("native memory daemon connection failed: {error:#}");
                    }
                    state.release_connection();
                });
            }
            _ = maintenance.tick() => {
                state.expire_sessions();
                state.registry.schedule_model_switches();
                if !state.drain_requested.load(Ordering::Acquire) {
                    state.registry.schedule_maintenance(maintenance_interval);
                }
                state.registry.evict_idle(project_idle).await;
                if state.drain_requested.load(Ordering::Acquire) && !state.is_busy() {
                    let not_before = *state
                        .drain_not_before
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if not_before.is_none_or(|deadline| Instant::now() >= deadline) {
                        break;
                    }
                }
                let idle = state
                    .last_activity
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .elapsed();
                if !state.is_busy() && idle >= daemon_idle {
                    let _admission = state
                        .admission
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !state.is_busy() {
                        state.drain_requested.store(true, Ordering::Release);
                        break;
                    }
                }
            }
        }
    }

    state.registry.close_all().await;
    drop(endpoint_guard);
    Ok(())
}

async fn serve_connection(stream: UnixStream, state: Arc<DaemonState>) -> Result<()> {
    let connection_id = opaque_id("connection");
    let connection_closed = Arc::new(AtomicBool::new(false));
    let (mut reader, mut writer) = stream.into_split();
    let (responses, mut response_queue) = mpsc::channel::<DaemonResponse>(64);
    let writer_task = tokio::spawn(async move {
        while let Some(response) = response_queue.recv().await {
            write_frame(&mut writer, &response, MAX_RESPONSE_BYTES).await?;
        }
        Result::<()>::Ok(())
    });
    let outstanding = Arc::new(Semaphore::new(MAX_OUTSTANDING_CALLS_PER_CONNECTION));
    let control_outstanding = Arc::new(Semaphore::new(8));

    loop {
        let frame = match read_frame(&mut reader, MAX_REQUEST_BYTES).await {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => {
                let _ = responses
                    .send(error_response(
                        "",
                        DaemonStatusCode::InvalidArgument,
                        format!("{error:#}"),
                    ))
                    .await;
                break;
            }
        };
        let mut request = match DaemonRequest::decode(frame.as_slice()) {
            Ok(request) => request,
            Err(error) => {
                let _ = responses
                    .send(error_response(
                        "",
                        DaemonStatusCode::InvalidArgument,
                        format!("invalid Protobuf daemon request: {error}"),
                    ))
                    .await;
                continue;
            }
        };
        if let Some(error) = validate_request_envelope(&request) {
            let _ = responses.send(error).await;
            continue;
        }
        let is_control = matches!(
            request.body,
            Some(daemon_request::Body::Heartbeat(_))
                | Some(daemon_request::Body::CancelCall(_))
                | Some(daemon_request::Body::ReleaseProject(_))
        );
        let semaphore = if is_control {
            &control_outstanding
        } else {
            &outstanding
        };
        let permit = match Arc::clone(semaphore).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                let _ = responses
                    .send(error_response(
                        &request.request_id,
                        DaemonStatusCode::ResourceExhausted,
                        "connection has too many outstanding calls",
                    ))
                    .await;
                continue;
            }
        };
        let request_id = request.request_id.clone();
        if matches!(request.body, Some(daemon_request::Body::ProjectCall(_))) {
            let Some(daemon_request::Body::ProjectCall(call)) = request.body.take() else {
                unreachable!("project call body was matched above")
            };
            match admit_project_call(&state, &connection_id, call) {
                Ok(admitted) => {
                    let responses = responses.clone();
                    let state = Arc::clone(&state);
                    tokio::spawn(async move {
                        let response = match execute_project_call(&state, admitted).await {
                            Ok(value) => {
                                ok_response(&request_id, daemon_response::Body::ProjectCall(value))
                            }
                            Err(error) => error_response(&request_id, error.code, error.message),
                        };
                        let _ = responses.send(response).await;
                        drop(permit);
                    });
                }
                Err(error) => {
                    let _ = responses
                        .send(error_response(&request_id, error.code, error.message))
                        .await;
                    drop(permit);
                }
            }
            continue;
        }
        if matches!(request.body, Some(daemon_request::Body::CancelCall(_))) {
            let Some(daemon_request::Body::CancelCall(cancel)) = request.body.take() else {
                unreachable!("cancel call body was matched above")
            };
            let response = match cancel_call(&state, &connection_id, cancel) {
                Ok(value) => ok_response(&request_id, daemon_response::Body::CancelCall(value)),
                Err(error) => error_response(&request_id, error.code, error.message),
            };
            let _ = responses.send(response).await;
            drop(permit);
            continue;
        }
        let synchronous = match request.body.take() {
            Some(daemon_request::Body::GetDaemonInfo(_)) => Some(Ok(
                daemon_response::Body::GetDaemonInfo(daemon_info(&state)),
            )),
            Some(daemon_request::Body::OpenSession(hello)) => Some(
                open_session(&state, &connection_id, &connection_closed, hello)
                    .map(daemon_response::Body::OpenSession),
            ),
            Some(daemon_request::Body::Heartbeat(heartbeat)) => Some(
                heartbeat_session(
                    &state,
                    &connection_id,
                    heartbeat.daemon_instance_id,
                    heartbeat.session_id,
                )
                .map(daemon_response::Body::Heartbeat),
            ),
            Some(daemon_request::Body::ReleaseProject(release)) => Some(
                release_project(
                    &state,
                    &connection_id,
                    release.daemon_instance_id,
                    release.session_id,
                    release.project_handle,
                    release.lease_id,
                )
                .map(daemon_response::Body::ReleaseProject),
            ),
            Some(daemon_request::Body::RequestDrain(drain)) => Some(
                request_drain(&state, drain.expected_daemon_instance_id)
                    .map(daemon_response::Body::RequestDrain),
            ),
            body => {
                request.body = body;
                None
            }
        };
        if let Some(result) = synchronous {
            let response = match result {
                Ok(body) => ok_response(&request_id, body),
                Err(error) => error_response(&request_id, error.code, error.message),
            };
            let _ = responses.send(response).await;
            drop(permit);
            continue;
        }
        let responses = responses.clone();
        let state = Arc::clone(&state);
        let connection_id = connection_id.clone();
        let connection_closed = Arc::clone(&connection_closed);
        tokio::spawn(async move {
            let response = handle_request(state, &connection_id, &connection_closed, request).await;
            let _ = responses.send(response).await;
            drop(permit);
        });
    }

    connection_closed.store(true, Ordering::Release);
    state.close_connection(&connection_id);
    drop(responses);
    writer_task
        .await
        .context("daemon response writer task panicked")??;
    Ok(())
}

fn validate_request_envelope(request: &DaemonRequest) -> Option<DaemonResponse> {
    if request.request_id.is_empty() {
        return Some(error_response(
            "",
            DaemonStatusCode::InvalidArgument,
            "daemon request_id is required",
        ));
    }
    if request.request_id.len() > MAX_OPAQUE_ID_BYTES {
        return Some(error_response(
            &request.request_id,
            DaemonStatusCode::InvalidArgument,
            "daemon request_id is too long",
        ));
    }
    (request.protocol_generation != DAEMON_PROTOCOL_GENERATION).then(|| {
        error_response(
            &request.request_id,
            DaemonStatusCode::FailedPrecondition,
            format!(
                "daemon protocol generation mismatch: client sent {}, daemon supports {}",
                request.protocol_generation, DAEMON_PROTOCOL_GENERATION
            ),
        )
    })
}

async fn handle_request(
    state: Arc<DaemonState>,
    connection_id: &str,
    connection_closed: &AtomicBool,
    request: DaemonRequest,
) -> DaemonResponse {
    let request_id = request.request_id.clone();
    if request_id.is_empty() {
        return error_response(
            "",
            DaemonStatusCode::InvalidArgument,
            "daemon request_id is required",
        );
    }
    if request.protocol_generation != DAEMON_PROTOCOL_GENERATION {
        return error_response(
            &request_id,
            DaemonStatusCode::FailedPrecondition,
            format!(
                "daemon protocol generation mismatch: client sent {}, daemon supports {}",
                request.protocol_generation, DAEMON_PROTOCOL_GENERATION
            ),
        );
    }
    let result = match request.body {
        Some(daemon_request::Body::GetDaemonInfo(_)) => {
            Ok(daemon_response::Body::GetDaemonInfo(daemon_info(&state)))
        }
        Some(daemon_request::Body::OpenSession(hello)) => {
            open_session(&state, connection_id, connection_closed, hello)
                .map(daemon_response::Body::OpenSession)
        }
        Some(daemon_request::Body::Heartbeat(heartbeat)) => heartbeat_session(
            &state,
            connection_id,
            heartbeat.daemon_instance_id,
            heartbeat.session_id,
        )
        .map(daemon_response::Body::Heartbeat),
        Some(daemon_request::Body::AcquireProject(acquire)) => {
            acquire_project(&state, connection_id, acquire)
                .await
                .map(daemon_response::Body::AcquireProject)
        }
        Some(daemon_request::Body::ProjectCall(call)) => project_call(&state, connection_id, call)
            .await
            .map(daemon_response::Body::ProjectCall),
        Some(daemon_request::Body::ReleaseProject(release)) => release_project(
            &state,
            connection_id,
            release.daemon_instance_id,
            release.session_id,
            release.project_handle,
            release.lease_id,
        )
        .map(daemon_response::Body::ReleaseProject),
        Some(daemon_request::Body::CancelCall(cancel)) => {
            cancel_call(&state, connection_id, cancel).map(daemon_response::Body::CancelCall)
        }
        Some(daemon_request::Body::RequestDrain(drain)) => {
            request_drain(&state, drain.expected_daemon_instance_id)
                .map(daemon_response::Body::RequestDrain)
        }
        None => Err(DaemonFailure::new(
            DaemonStatusCode::InvalidArgument,
            "daemon request body is required",
        )),
    };

    match result {
        Ok(body) => ok_response(&request_id, body),
        Err(error) => error_response(&request_id, error.code, error.message),
    }
}

fn daemon_info(state: &DaemonState) -> GetDaemonInfoResponse {
    GetDaemonInfoResponse {
        daemon_instance_id: state.instance_id.clone(),
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        minimum_protocol_generation: DAEMON_PROTOCOL_GENERATION,
        maximum_protocol_generation: DAEMON_PROTOCOL_GENERATION,
        domain_schema_generation: DOMAIN_SCHEMA_GENERATION,
        capabilities: capabilities(),
        pid: std::process::id(),
    }
}

fn open_session(
    state: &DaemonState,
    connection_id: &str,
    connection_closed: &AtomicBool,
    hello: OpenSessionRequest,
) -> DaemonResult<OpenSessionResponse> {
    let _admission = state
        .admission
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if connection_closed.load(Ordering::Acquire) {
        return Err(DaemonFailure::new(
            DaemonStatusCode::Cancelled,
            "daemon connection closed before session admission",
        ));
    }
    if state.drain_requested.load(Ordering::Acquire) {
        return Err(DaemonFailure::new(
            DaemonStatusCode::Unavailable,
            "daemon is draining",
        ));
    }
    if hello.minimum_protocol_generation > DAEMON_PROTOCOL_GENERATION
        || hello.maximum_protocol_generation < DAEMON_PROTOCOL_GENERATION
    {
        return Err(DaemonFailure::new(
            DaemonStatusCode::FailedPrecondition,
            "client and daemon protocol generations do not overlap",
        ));
    }
    if hello.domain_schema_generation != DOMAIN_SCHEMA_GENERATION {
        return Err(DaemonFailure::new(
            DaemonStatusCode::FailedPrecondition,
            "client and daemon domain schema generations do not match",
        ));
    }
    if hello.client_instance_id.is_empty() {
        return Err(DaemonFailure::new(
            DaemonStatusCode::InvalidArgument,
            "client_instance_id is required",
        ));
    }
    let session_id = opaque_id("session");
    let mut sessions = state
        .sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if sessions.len() >= MAX_SESSIONS {
        return Err(DaemonFailure::new(
            DaemonStatusCode::ResourceExhausted,
            "daemon session capacity is exhausted",
        ));
    }
    if sessions
        .values()
        .any(|session| session.connection_id == connection_id)
    {
        return Err(DaemonFailure::new(
            DaemonStatusCode::FailedPrecondition,
            "this connection already owns a daemon session",
        ));
    }
    sessions.insert(
        session_id.clone(),
        Session {
            connection_id: connection_id.to_string(),
            last_heartbeat: Instant::now(),
            projects: HashMap::new(),
            calls: HashMap::new(),
        },
    );
    drop(sessions);
    state.touch();
    Ok(OpenSessionResponse {
        daemon_instance_id: state.instance_id.clone(),
        session_id,
        selected_protocol_generation: DAEMON_PROTOCOL_GENERATION,
        domain_schema_generation: DOMAIN_SCHEMA_GENERATION,
        capabilities: capabilities(),
        heartbeat_interval_seconds: HEARTBEAT_INTERVAL_SECONDS,
        lease_ttl_seconds: LEASE_TTL_SECONDS,
    })
}

fn heartbeat_session(
    state: &DaemonState,
    connection_id: &str,
    daemon_instance_id: String,
    session_id: String,
) -> DaemonResult<SessionHeartbeatResponse> {
    validate_daemon_instance(state, &daemon_instance_id)?;
    let mut sessions = state
        .sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let session = sessions.get_mut(&session_id).ok_or_else(|| {
        DaemonFailure::new(DaemonStatusCode::NotFound, "daemon session was not found")
    })?;
    validate_session_connection(session, connection_id)?;
    session.last_heartbeat = Instant::now();
    Ok(SessionHeartbeatResponse {
        server_monotonic_ms: monotonic_ms(),
    })
}

async fn acquire_project(
    state: &DaemonState,
    connection_id: &str,
    request: AcquireProjectRequest,
) -> DaemonResult<AcquireProjectResponse> {
    validate_daemon_instance(state, &request.daemon_instance_id)?;
    if state.drain_requested.load(Ordering::Acquire) {
        return Err(DaemonFailure::new(
            DaemonStatusCode::Unavailable,
            "daemon is draining",
        ));
    }
    let config = config_from_acquire(&request).map_err(|error| {
        DaemonFailure::new(DaemonStatusCode::InvalidArgument, format!("{error:#}"))
    })?;
    let fingerprint_config = config.clone();
    let (store_key, fingerprint) = tokio::task::spawn_blocking(move || {
        Ok::<_, anyhow::Error>((
            fingerprint_config.canonical_store_key()?,
            fingerprint_config.actor_compatibility_fingerprint()?,
        ))
    })
    .await
    .map_err(|error| internal_failure(anyhow!("configuration fingerprint task failed: {error}")))?
    .map_err(|error| DaemonFailure::new(DaemonStatusCode::InvalidArgument, format!("{error:#}")))?;
    let existing = {
        let sessions = state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = sessions.get(&request.session_id).ok_or_else(|| {
            DaemonFailure::new(DaemonStatusCode::NotFound, "daemon session was not found")
        })?;
        validate_session_connection(session, connection_id)?;
        if let Some(existing) = session.projects.get(&store_key) {
            if existing.actor.fingerprint() != fingerprint {
                return Err(DaemonFailure::new(
                    DaemonStatusCode::FailedPrecondition,
                    "project configuration does not match the existing session lease",
                ));
            }
            Some((
                Arc::clone(&existing.actor),
                acquire_response(existing, &config, &store_key),
            ))
        } else {
            None
        }
    };
    if let Some((actor, response)) = existing {
        actor.wait_ready().await.map_err(internal_failure)?;
        return Ok(response);
    }

    let actor = state
        .registry
        .acquire(config.clone(), store_key.clone(), fingerprint.clone())
        .await
        .map_err(|error| {
            let message = format!("{error:#}");
            let code = if message.contains("limit") {
                DaemonStatusCode::ResourceExhausted
            } else if message.contains("does not match") {
                DaemonStatusCode::FailedPrecondition
            } else {
                DaemonStatusCode::Internal
            };
            DaemonFailure::new(code, message)
        })?;
    let lease = ProjectLease {
        handle: opaque_id("project"),
        lease_id: opaque_id("lease"),
        actor,
    };
    let mut sessions = state
        .sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let session = sessions.get_mut(&request.session_id).ok_or_else(|| {
        lease.actor.release_lease();
        DaemonFailure::new(
            DaemonStatusCode::NotFound,
            "daemon session expired while opening project",
        )
    })?;
    validate_session_connection(session, connection_id)?;
    if let Some(existing) = session.projects.get(&store_key) {
        lease.actor.release_lease();
        return Ok(acquire_response(existing, &config, &store_key));
    }
    let response = acquire_response(&lease, &config, &store_key);
    session.projects.insert(store_key, lease);
    state.touch();
    Ok(response)
}

struct AdmittedProjectCall {
    actor: Arc<ProjectActor>,
    domain_request: ProjectRequest,
    deadline: Instant,
    call_id: String,
    call_state: Arc<AtomicU8>,
}

fn admit_project_call(
    state: &DaemonState,
    connection_id: &str,
    request: ProjectCallRequest,
) -> DaemonResult<AdmittedProjectCall> {
    validate_daemon_instance(state, &request.daemon_instance_id)?;
    if request.call_id.is_empty() {
        return Err(DaemonFailure::new(
            DaemonStatusCode::InvalidArgument,
            "project call_id is required",
        ));
    }
    if request.call_id.len() > MAX_OPAQUE_ID_BYTES {
        return Err(DaemonFailure::new(
            DaemonStatusCode::InvalidArgument,
            "project call_id is too long",
        ));
    }
    if request.timeout_ms == 0 || request.timeout_ms > MAX_CALL_TIMEOUT_MS {
        return Err(DaemonFailure::new(
            DaemonStatusCode::InvalidArgument,
            "project timeout_ms is outside the supported range",
        ));
    }
    let domain_request = match (
        request.request,
        request.model_request,
        request.graph_request,
    ) {
        (Some(request), None, None) => ProjectRequest::Memory(request),
        (None, Some(request), None) => ProjectRequest::Model(request),
        (None, None, Some(request)) => ProjectRequest::Graph(request),
        _ => {
            return Err(DaemonFailure::new(
                DaemonStatusCode::InvalidArgument,
                "project call must contain exactly one memory, model, or graph request",
            ));
        }
    };
    validate_project_request(&domain_request).map_err(|error| {
        DaemonFailure::new(DaemonStatusCode::InvalidArgument, format!("{error:#}"))
    })?;
    if let ProjectRequest::Memory(request) = &domain_request
        && Method::try_from(request.method).ok() == Some(Method::Shutdown)
    {
        return Err(DaemonFailure::new(
            DaemonStatusCode::FailedPrecondition,
            "shared daemon project calls cannot request global shutdown",
        ));
    }
    let (actor, call_state) = {
        let mut sessions = state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = sessions.get_mut(&request.session_id).ok_or_else(|| {
            DaemonFailure::new(DaemonStatusCode::NotFound, "daemon session was not found")
        })?;
        validate_session_connection(session, connection_id)?;
        if session.calls.len() >= MAX_CALL_IDS_PER_SESSION {
            session.calls.retain(|_, call| {
                !matches!(
                    call.state.load(Ordering::Acquire),
                    CALL_COMPLETED | CALL_CANCELLED
                )
            });
        }
        if session.calls.len() >= MAX_CALL_IDS_PER_SESSION {
            return Err(DaemonFailure::new(
                DaemonStatusCode::ResourceExhausted,
                "session call ID capacity is exhausted; reconnect before issuing more calls",
            ));
        }
        if session.calls.contains_key(&request.call_id) {
            return Err(DaemonFailure::new(
                DaemonStatusCode::FailedPrecondition,
                "duplicate project call_id was already admitted for this session",
            ));
        }
        let actor = session
            .projects
            .values()
            .find(|lease| {
                lease.handle == request.project_handle && lease.lease_id == request.lease_id
            })
            .map(|lease| Arc::clone(&lease.actor))
            .ok_or_else(|| {
                DaemonFailure::new(DaemonStatusCode::NotFound, "project lease was not found")
            })?;
        let call_state = Arc::new(AtomicU8::new(CALL_QUEUED));
        session.calls.insert(
            request.call_id.clone(),
            ActiveCall {
                project_handle: request.project_handle.clone(),
                lease_id: request.lease_id.clone(),
                state: Arc::clone(&call_state),
            },
        );
        (actor, call_state)
    };
    Ok(AdmittedProjectCall {
        actor,
        domain_request,
        deadline: Instant::now() + Duration::from_millis(u64::from(request.timeout_ms)),
        call_id: request.call_id,
        call_state,
    })
}

async fn execute_project_call(
    state: &DaemonState,
    admitted: AdmittedProjectCall,
) -> DaemonResult<ProjectCallResponse> {
    enum Domain {
        Memory,
        Model,
        Graph,
    }
    let (request_id, domain) = match &admitted.domain_request {
        ProjectRequest::Memory(request) => (request.id, Domain::Memory),
        ProjectRequest::Model(request) => (request.id, Domain::Model),
        ProjectRequest::Graph(request) => (request.id, Domain::Graph),
    };
    let response = admitted
        .actor
        .call(
            admitted.domain_request,
            admitted.deadline,
            admitted.call_state,
        )
        .await;
    state.touch();
    let response = response.map_err(|error| {
        let message = format!("{error:#}");
        let code = if message.contains("deadline") {
            DaemonStatusCode::DeadlineExceeded
        } else if message.contains("cancelled before transaction start") {
            DaemonStatusCode::Cancelled
        } else {
            DaemonStatusCode::Internal
        };
        DaemonFailure::new(code, message)
    })?;
    match response {
        ProjectResponse::Memory(response) if matches!(domain, Domain::Memory) => {
            if response.id != request_id {
                return Err(DaemonFailure::new(
                    DaemonStatusCode::Internal,
                    "memory response id does not match the admitted request",
                ));
            }
            Ok(ProjectCallResponse {
                call_id: admitted.call_id,
                response: Some(response),
                model_response: None,
                graph_response: None,
            })
        }
        ProjectResponse::Model(response) if matches!(domain, Domain::Model) => {
            if response.id != request_id {
                return Err(DaemonFailure::new(
                    DaemonStatusCode::Internal,
                    "model response id does not match the admitted request",
                ));
            }
            Ok(ProjectCallResponse {
                call_id: admitted.call_id,
                response: None,
                model_response: Some(response),
                graph_response: None,
            })
        }
        ProjectResponse::Graph(response) if matches!(domain, Domain::Graph) => {
            if response.id != request_id {
                return Err(DaemonFailure::new(
                    DaemonStatusCode::Internal,
                    "graph response id does not match the admitted request",
                ));
            }
            Ok(ProjectCallResponse {
                call_id: admitted.call_id,
                response: None,
                model_response: None,
                graph_response: Some(response),
            })
        }
        _ => Err(DaemonFailure::new(
            DaemonStatusCode::Internal,
            "project actor returned a response for the wrong request domain",
        )),
    }
}

async fn project_call(
    state: &DaemonState,
    connection_id: &str,
    request: ProjectCallRequest,
) -> DaemonResult<ProjectCallResponse> {
    let admitted = admit_project_call(state, connection_id, request)?;
    execute_project_call(state, admitted).await
}

fn release_project(
    state: &DaemonState,
    connection_id: &str,
    daemon_instance_id: String,
    session_id: String,
    project_handle: String,
    lease_id: String,
) -> DaemonResult<ReleaseProjectResponse> {
    validate_daemon_instance(state, &daemon_instance_id)?;
    let released = {
        let mut sessions = state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(session) = sessions.get_mut(&session_id) else {
            return Ok(ReleaseProjectResponse { released: false });
        };
        validate_session_connection(session, connection_id)?;
        for call in session.calls.values() {
            if call.project_handle == project_handle && call.lease_id == lease_id {
                let _ = call.state.compare_exchange(
                    CALL_QUEUED,
                    CALL_CANCELLED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
        }
        let key = session.projects.iter().find_map(|(key, lease)| {
            (lease.handle == project_handle && lease.lease_id == lease_id).then_some(key.clone())
        });
        key.and_then(|key| session.projects.remove(&key))
    };
    if let Some(lease) = released {
        lease.actor.release_lease();
        state.touch();
        Ok(ReleaseProjectResponse { released: true })
    } else {
        Ok(ReleaseProjectResponse { released: false })
    }
}

fn cancel_call(
    state: &DaemonState,
    connection_id: &str,
    request: CancelCallRequest,
) -> DaemonResult<CancelCallResponse> {
    validate_daemon_instance(state, &request.daemon_instance_id)?;
    let sessions = state
        .sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let session = sessions.get(&request.session_id).ok_or_else(|| {
        DaemonFailure::new(DaemonStatusCode::NotFound, "daemon session was not found")
    })?;
    validate_session_connection(session, connection_id)?;
    let Some(call) = session.calls.get(&request.call_id) else {
        return Ok(CancelCallResponse {
            outcome: CancelOutcome::NotFound as i32,
        });
    };
    if call.project_handle != request.project_handle || call.lease_id != request.lease_id {
        return Err(DaemonFailure::new(
            DaemonStatusCode::FailedPrecondition,
            "project call belongs to a different lease",
        ));
    }
    let outcome = match call.state.compare_exchange(
        CALL_QUEUED,
        CALL_CANCELLED,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => CancelOutcome::CancelledBeforeStart,
        Err(CALL_RUNNING) => CancelOutcome::AlreadyStarted,
        Err(CALL_COMPLETED) => CancelOutcome::Completed,
        Err(CALL_CANCELLED) => CancelOutcome::CancelledBeforeStart,
        Err(_) => CancelOutcome::NotFound,
    };
    Ok(CancelCallResponse {
        outcome: outcome as i32,
    })
}

fn request_drain(
    state: &DaemonState,
    expected_daemon_instance_id: String,
) -> DaemonResult<RequestDrainResponse> {
    validate_daemon_instance(state, &expected_daemon_instance_id)?;
    let _admission = state
        .admission
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.is_busy() {
        return Ok(RequestDrainResponse {
            outcome: DrainOutcome::Busy as i32,
            retry_after_ms: 1_000,
        });
    }
    state.drain_requested.store(true, Ordering::Release);
    *state
        .drain_not_before
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some(Instant::now() + Duration::from_secs(1));
    state.touch();
    Ok(RequestDrainResponse {
        outcome: DrainOutcome::Accepted as i32,
        retry_after_ms: 0,
    })
}

fn config_from_acquire(request: &AcquireProjectRequest) -> Result<MemoryConfig> {
    let root = if request.project_root.is_empty() {
        &request.worktree
    } else {
        &request.project_root
    };
    anyhow::ensure!(!root.is_empty(), "project_root is required");
    let mut embedding = if let Some(profile_id) = &request.initial_profile_id {
        crate::model::embedding_config_for_profile(profile_id, &EmbeddingConfig::default())?
    } else {
        EmbeddingConfig::default()
    };
    if let Some(input) = &request.embedding {
        embedding.model_path = input.local_model_path.as_deref().map(PathBuf::from);
        if let Some(value) = &input.repository {
            embedding.repo.clone_from(value);
        }
        if let Some(value) = &input.revision {
            embedding.revision.clone_from(value);
        }
        if let Some(value) = &input.filename {
            embedding.filename.clone_from(value);
        }
        if let Some(value) = &input.pooling {
            embedding.pooling.clone_from(value);
        }
        if let Some(value) = &input.attention {
            embedding.attention.clone_from(value);
        }
        if let Some(value) = &input.query_template {
            embedding.query_template.clone_from(value);
        }
        if let Some(value) = &input.passage_template {
            embedding.passage_template.clone_from(value);
        }
        if let Some(value) = input.add_bos {
            embedding.add_bos = value;
        }
        if let Some(value) = input.append_eos {
            embedding.append_eos = value;
        }
        if let Some(value) = input.normalize {
            embedding.normalize = value;
        }
        if let Some(value) = input.dimension {
            embedding.dimension = Some(value as usize);
        }
        if let Some(value) = input.context_size {
            embedding.context_size = value;
        }
        if input.threads.is_some() {
            embedding.threads = input.threads;
        }
        if input.gpu_layers.is_some() {
            embedding.gpu_layers = input.gpu_layers;
        }
    }
    let available = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let daemon_default = i32::try_from((available / 2).max(1)).unwrap_or(i32::MAX);
    embedding.threads = Some(
        embedding
            .threads
            .unwrap_or(daemon_default)
            .min(daemon_default),
    );
    if embedding.model_path.is_none() {
        anyhow::ensure!(
            embedding.revision.len() == 40
                && embedding
                    .revision
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit()),
            "daemon Hugging Face embedding revision must be an immutable 40-character commit SHA"
        );
    }
    embedding.validate()?;
    let mut config = MemoryConfig::for_daemon(
        PathBuf::from(root),
        request.data_dir.as_deref().map(PathBuf::from),
        request.model_cache.as_deref().map(PathBuf::from),
        embedding,
    )?;
    if let Some(active) = crate::embedding_generation::ActiveEmbedding::load(
        &config.active_embedding_path(),
        config.project_id(),
    )? {
        if let Some(expected) = &request.expected_profile_id {
            anyhow::ensure!(
                expected == &active.profile_id,
                "expected active profile {expected}, found {}",
                active.profile_id
            );
        }
        if active.profile_id != "legacy-custom" {
            let persisted =
                crate::model::embedding_config_for_profile(&active.profile_id, config.embedding())?;
            config.set_embedding(persisted);
        } else {
            let store = crate::embedding_generation::ModelSwitchStore::load(
                &config.model_switch_path(),
                config.project_id(),
            )?;
            if let Some(embedding) = store
                .current
                .iter()
                .chain(store.history.iter().rev())
                .find(|job| {
                    job.phase == crate::embedding_generation::SwitchPhase::Succeeded
                        && job.target_generation_id == active.generation_id
                        && job.target_profile_id == active.profile_id
                })
                .and_then(|job| job.target_embedding.clone())
            {
                config.set_embedding(embedding);
            }
            anyhow::ensure!(
                active.profile_fingerprint == config.embedding_profile_fingerprint()?,
                "requested custom embedding does not match the persisted active profile"
            );
        }
    } else if let Some(expected) = &request.expected_profile_id {
        anyhow::ensure!(
            expected == &crate::model::configured_profile_id(config.embedding()),
            "expected active profile {expected}, found {}",
            crate::model::configured_profile_id(config.embedding())
        );
    }
    Ok(config)
}

fn acquire_response(
    lease: &ProjectLease,
    config: &MemoryConfig,
    store_key: &Path,
) -> AcquireProjectResponse {
    let active = crate::embedding_generation::ActiveEmbedding::load(
        &config.active_embedding_path(),
        config.project_id(),
    )
    .ok()
    .flatten();
    AcquireProjectResponse {
        project_handle: lease.handle.clone(),
        lease_id: lease.lease_id.clone(),
        canonical_project_id: config.project_id().to_string(),
        store_key_hash: crate::config::hash_hex(store_key.as_os_str().as_encoded_bytes()),
        active_profile_id: active.as_ref().map_or_else(
            || crate::model::configured_profile_id(config.embedding()),
            |value| value.profile_id.clone(),
        ),
        active_generation_id: active
            .map_or_else(|| "legacy".to_string(), |value| value.generation_id),
    }
}

fn validate_daemon_instance(state: &DaemonState, value: &str) -> DaemonResult<()> {
    if value == state.instance_id {
        Ok(())
    } else {
        Err(DaemonFailure::new(
            DaemonStatusCode::FailedPrecondition,
            "daemon instance changed; reconnect and reacquire project leases",
        ))
    }
}

fn validate_session_connection(session: &Session, connection_id: &str) -> DaemonResult<()> {
    if session.connection_id == connection_id {
        Ok(())
    } else {
        Err(DaemonFailure::new(
            DaemonStatusCode::FailedPrecondition,
            "daemon session belongs to a different connection",
        ))
    }
}

fn capabilities() -> Vec<String> {
    vec![
        "framed-protobuf-uds".to_string(),
        "project-actors".to_string(),
        "session-leases".to_string(),
        "unary-deadlines".to_string(),
        "call-cancellation".to_string(),
        "no-mutation-replay".to_string(),
        "model-profile-catalog-v1".to_string(),
        "model-switch-preflight-v1".to_string(),
        "knowledge-graph-v1".to_string(),
        "graph-extraction-prepare-v1".to_string(),
        "graph-idempotent-upsert-v1".to_string(),
        "graph-run-receipts-v1".to_string(),
        "graph-search-v1".to_string(),
        "graph-export-v1".to_string(),
        "graph-rrf-fusion-v1".to_string(),
        "graph-durable-extraction-jobs-v1".to_string(),
        "daemon-periodic-optimize-v1".to_string(),
        "durable-model-switch-v1".to_string(),
        "embedding-generation-cutover-v1".to_string(),
        "model-switch-cli-v1".to_string(),
        "graph-shared-project-actor-v1".to_string(),
    ]
}

fn cancel_queued_calls(calls: &HashMap<String, ActiveCall>) {
    for call in calls.values() {
        let _ = call.state.compare_exchange(
            CALL_QUEUED,
            CALL_CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

fn ok_response(request_id: &str, body: daemon_response::Body) -> DaemonResponse {
    DaemonResponse {
        request_id: request_id.to_string(),
        status: Some(DaemonStatus {
            code: DaemonStatusCode::Ok as i32,
            message: String::new(),
            retry_after_ms: 0,
        }),
        body: Some(body),
    }
}

fn error_response(
    request_id: &str,
    code: DaemonStatusCode,
    message: impl Into<String>,
) -> DaemonResponse {
    DaemonResponse {
        request_id: request_id.to_string(),
        status: Some(DaemonStatus {
            code: code as i32,
            message: message.into(),
            retry_after_ms: u32::from(code == DaemonStatusCode::ResourceExhausted) * 1_000,
        }),
        body: None,
    }
}

#[derive(Debug)]
struct DaemonFailure {
    code: DaemonStatusCode,
    message: String,
}

impl DaemonFailure {
    fn new(code: DaemonStatusCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

type DaemonResult<T> = std::result::Result<T, DaemonFailure>;

fn internal_failure(error: anyhow::Error) -> DaemonFailure {
    DaemonFailure::new(DaemonStatusCode::Internal, format!("{error:#}"))
}

async fn read_frame(
    reader: &mut (impl AsyncRead + Unpin),
    max_bytes: usize,
) -> Result<Option<Vec<u8>>> {
    let Some(length) = read_varint(reader).await? else {
        return Ok(None);
    };
    let length = usize::try_from(length).context("Protobuf frame length exceeds usize")?;
    anyhow::ensure!(
        length <= max_bytes,
        "daemon request exceeds {max_bytes} bytes"
    );
    let mut frame = vec![0; length];
    reader
        .read_exact(&mut frame)
        .await
        .context("truncated Protobuf daemon frame")?;
    Ok(Some(frame))
}

async fn read_varint(reader: &mut (impl AsyncRead + Unpin)) -> Result<Option<u64>> {
    let mut value = 0_u64;
    for shift in (0..70).step_by(7) {
        let byte = match reader.read_u8().await {
            Ok(byte) => byte,
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof && shift == 0 => {
                return Ok(None);
            }
            Err(error) => return Err(error).context("cannot read daemon frame length"),
        };
        let payload = u64::from(byte & 0x7f);
        anyhow::ensure!(
            shift < 64 || payload <= 1,
            "invalid Protobuf daemon frame length"
        );
        value |= payload << shift.min(63);
        if byte & 0x80 == 0 {
            return Ok(Some(value));
        }
    }
    Err(anyhow!("invalid Protobuf daemon frame length"))
}

async fn write_frame(
    writer: &mut (impl AsyncWrite + Unpin),
    message: &impl Message,
    max_bytes: usize,
) -> Result<()> {
    let encoded_len = message.encoded_len();
    anyhow::ensure!(
        encoded_len <= max_bytes,
        "daemon response exceeds {max_bytes} bytes"
    );
    let mut frame = Vec::with_capacity(encoded_len + 10);
    message.encode_length_delimited(&mut frame)?;
    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}

fn opaque_id(prefix: &str) -> String {
    let counter = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{prefix}-{:x}-{nanos:x}-{counter:x}", std::process::id())
}

fn monotonic_ms() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    u64::try_from(START.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn configured_duration(name: &str, default_seconds: u64) -> Duration {
    let seconds = std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default_seconds);
    Duration::from_secs(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_proto::{ListModelProfilesRequest, ModelRequest, model_request};

    fn hello() -> OpenSessionRequest {
        OpenSessionRequest {
            client_instance_id: "client-a".to_string(),
            minimum_protocol_generation: DAEMON_PROTOCOL_GENERATION,
            maximum_protocol_generation: DAEMON_PROTOCOL_GENERATION,
            domain_schema_generation: DOMAIN_SCHEMA_GENERATION,
            plugin_version: "test".to_string(),
        }
    }

    fn project_call_request(
        state: &DaemonState,
        request: Option<crate::memory_proto::Request>,
        model_request: Option<ModelRequest>,
    ) -> ProjectCallRequest {
        ProjectCallRequest {
            daemon_instance_id: state.instance_id.clone(),
            session_id: "session-a".to_string(),
            project_handle: "project-a".to_string(),
            lease_id: "lease-a".to_string(),
            call_id: "call-a".to_string(),
            timeout_ms: 1_000,
            request,
            model_request,
            graph_request: None,
        }
    }

    #[test]
    fn session_credentials_are_bound_to_the_opening_connection() {
        let state = DaemonState::new();
        let session = open_session(&state, "connection-a", &AtomicBool::new(false), hello())
            .expect("open session");

        let error = heartbeat_session(
            &state,
            "connection-b",
            state.instance_id.clone(),
            session.session_id,
        )
        .expect_err("reject foreign connection");
        assert_eq!(error.code, DaemonStatusCode::FailedPrecondition);
        assert!(error.message.contains("different connection"));
    }

    #[test]
    fn cancellation_tombstones_a_queued_call_before_actor_admission() {
        let state = DaemonState::new();
        let session = open_session(&state, "connection-a", &AtomicBool::new(false), hello())
            .expect("open session");
        let call_state = Arc::new(AtomicU8::new(CALL_QUEUED));
        state
            .sessions
            .lock()
            .expect("sessions")
            .get_mut(&session.session_id)
            .expect("session")
            .calls
            .insert(
                "call-a".to_string(),
                ActiveCall {
                    project_handle: "project-a".to_string(),
                    lease_id: "lease-a".to_string(),
                    state: Arc::clone(&call_state),
                },
            );

        let response = cancel_call(
            &state,
            "connection-a",
            CancelCallRequest {
                daemon_instance_id: state.instance_id.clone(),
                session_id: session.session_id,
                project_handle: "project-a".to_string(),
                lease_id: "lease-a".to_string(),
                call_id: "call-a".to_string(),
            },
        )
        .expect("cancel call");
        assert_eq!(response.outcome, CancelOutcome::CancelledBeforeStart as i32);
        assert_eq!(call_state.load(Ordering::Acquire), CALL_CANCELLED);
    }

    #[test]
    fn project_call_requires_exactly_one_domain_request() {
        let state = DaemonState::new();
        let memory_request = crate::memory_proto::Request {
            id: 1,
            method: Method::Status as i32,
            params: None,
        };
        let model_request = ModelRequest {
            id: 2,
            operation: Some(model_request::Operation::ListProfiles(
                ListModelProfilesRequest {},
            )),
        };

        let neither = match admit_project_call(
            &state,
            "connection-a",
            project_call_request(&state, None, None),
        ) {
            Err(error) => error,
            Ok(_) => panic!("accepted missing domain request"),
        };
        assert_eq!(neither.code, DaemonStatusCode::InvalidArgument);

        let both = match admit_project_call(
            &state,
            "connection-a",
            project_call_request(&state, Some(memory_request), Some(model_request)),
        ) {
            Err(error) => error,
            Ok(_) => panic!("accepted ambiguous domain request"),
        };
        assert_eq!(both.code, DaemonStatusCode::InvalidArgument);
    }

    #[test]
    fn shutdown_detection_only_applies_to_memory_requests() {
        let state = DaemonState::new();
        let shutdown = match admit_project_call(
            &state,
            "connection-a",
            project_call_request(
                &state,
                Some(crate::memory_proto::Request {
                    id: 1,
                    method: Method::Shutdown as i32,
                    params: None,
                }),
                None,
            ),
        ) {
            Err(error) => error,
            Ok(_) => panic!("accepted shared-daemon shutdown"),
        };
        assert_eq!(shutdown.code, DaemonStatusCode::FailedPrecondition);

        let model = match admit_project_call(
            &state,
            "connection-a",
            project_call_request(
                &state,
                None,
                Some(ModelRequest {
                    id: 2,
                    operation: Some(model_request::Operation::ListProfiles(
                        ListModelProfilesRequest {},
                    )),
                }),
            ),
        ) {
            Err(error) => error,
            Ok(_) => panic!("accepted model request without a session"),
        };
        assert_eq!(model.code, DaemonStatusCode::NotFound);
    }

    #[test]
    fn accepted_drain_atomically_closes_session_admission() {
        let state = DaemonState::new();
        let response = request_drain(&state, state.instance_id.clone()).expect("request drain");
        assert_eq!(response.outcome, DrainOutcome::Accepted as i32);

        let error = open_session(&state, "connection-a", &AtomicBool::new(false), hello())
            .expect_err("reject session after drain");
        assert_eq!(error.code, DaemonStatusCode::Unavailable);
    }
}
