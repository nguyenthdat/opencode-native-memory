use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use prost::Message;
use tokio::sync::{mpsc, oneshot, watch};

use crate::MemoryConfig;
use crate::rpc::{ProjectRequest, ProjectResponse};

const PROJECT_QUEUE_CAPACITY: usize = 64;
const PROJECT_QUEUE_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const CALL_QUEUED: u8 = 0;
pub(crate) const CALL_RUNNING: u8 = 1;
pub(crate) const CALL_COMPLETED: u8 = 2;
pub(crate) const CALL_CANCELLED: u8 = 3;

struct QueuePermit {
    project_bytes: Arc<AtomicUsize>,
    global_bytes: Arc<AtomicUsize>,
    bytes: usize,
}

impl QueuePermit {
    fn reserve(
        project_bytes: Arc<AtomicUsize>,
        global_bytes: Arc<AtomicUsize>,
        bytes: usize,
        global_limit: usize,
    ) -> Result<Self> {
        reserve_bytes(&project_bytes, bytes, PROJECT_QUEUE_BYTES)
            .map_err(|_| anyhow!("project command byte capacity is exhausted"))?;
        if reserve_bytes(&global_bytes, bytes, global_limit).is_err() {
            project_bytes.fetch_sub(bytes, Ordering::AcqRel);
            return Err(anyhow!("daemon command byte capacity is exhausted"));
        }
        Ok(Self {
            project_bytes,
            global_bytes,
            bytes,
        })
    }
}

impl Drop for QueuePermit {
    fn drop(&mut self) {
        self.project_bytes.fetch_sub(self.bytes, Ordering::AcqRel);
        self.global_bytes.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

struct CommandPermit {
    _queue: QueuePermit,
    pending_commands: Arc<AtomicUsize>,
}

struct MaintenancePermit {
    pending_commands: Arc<AtomicUsize>,
    maintenance_queued: Arc<AtomicBool>,
}

impl Drop for MaintenancePermit {
    fn drop(&mut self) {
        self.pending_commands.fetch_sub(1, Ordering::AcqRel);
        self.maintenance_queued.store(false, Ordering::Release);
    }
}

impl Drop for CommandPermit {
    fn drop(&mut self) {
        self.pending_commands.fetch_sub(1, Ordering::AcqRel);
    }
}

fn reserve_bytes(counter: &AtomicUsize, bytes: usize, limit: usize) -> Result<(), ()> {
    counter
        .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current
                .checked_add(bytes)
                .filter(|updated| *updated <= limit)
        })
        .map(|_| ())
        .map_err(|_| ())
}

#[derive(Clone, Debug)]
enum ActorState {
    Opening,
    Ready,
    Draining,
    Failed(String),
    Closed,
}

enum ActorCommand {
    Call {
        request: Box<ProjectRequest>,
        deadline: Instant,
        command_permit: CommandPermit,
        call_state: Arc<AtomicU8>,
        reply: oneshot::Sender<Result<ProjectResponse, String>>,
    },
    Maintenance {
        permit: MaintenancePermit,
    },
    Stop,
}

struct ActorCloseGuard(watch::Sender<ActorState>);

impl Drop for ActorCloseGuard {
    fn drop(&mut self) {
        let _ = self.0.send(ActorState::Closed);
    }
}

pub(crate) struct ProjectActor {
    store_key: PathBuf,
    fingerprint: String,
    sender: mpsc::Sender<ActorCommand>,
    state: watch::Receiver<ActorState>,
    state_sender: watch::Sender<ActorState>,
    accepting: Mutex<bool>,
    leases: AtomicUsize,
    active_commands: AtomicUsize,
    pending_commands: Arc<AtomicUsize>,
    engine_initialized: AtomicBool,
    maintenance_queued: Arc<AtomicBool>,
    last_maintenance_attempt: Mutex<Option<Instant>>,
    failure: Mutex<Option<String>>,
    queued_bytes: Arc<AtomicUsize>,
    global_queued_bytes: Arc<AtomicUsize>,
    global_queue_limit: usize,
    last_activity: Mutex<Instant>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl ProjectActor {
    pub(crate) fn spawn(
        config: MemoryConfig,
        store_key: PathBuf,
        fingerprint: String,
        model_load_lock: Arc<Mutex<()>>,
        inference_lock: Arc<Mutex<()>>,
        global_queued_bytes: Arc<AtomicUsize>,
        global_queue_limit: usize,
    ) -> Result<Arc<Self>> {
        let (sender, mut receiver) = mpsc::channel(PROJECT_QUEUE_CAPACITY);
        let (state_sender, state) = watch::channel(ActorState::Opening);
        let actor = Arc::new(Self {
            store_key,
            fingerprint,
            sender,
            state,
            state_sender: state_sender.clone(),
            accepting: Mutex::new(true),
            leases: AtomicUsize::new(0),
            active_commands: AtomicUsize::new(0),
            pending_commands: Arc::new(AtomicUsize::new(0)),
            engine_initialized: AtomicBool::new(false),
            maintenance_queued: Arc::new(AtomicBool::new(false)),
            last_maintenance_attempt: Mutex::new(None),
            failure: Mutex::new(None),
            queued_bytes: Arc::new(AtomicUsize::new(0)),
            global_queued_bytes,
            global_queue_limit,
            last_activity: Mutex::new(Instant::now()),
            worker: Mutex::new(None),
        });

        let actor_for_thread = Arc::clone(&actor);
        let worker = std::thread::Builder::new()
            .name(format!(
                "memory-project-{}",
                &actor_for_thread.fingerprint[..12.min(actor_for_thread.fingerprint.len())]
            ))
            .spawn(move || {
                let _close_guard = ActorCloseGuard(state_sender.clone());
                let mut service =
                    crate::rpc::Service::new_with_locks(config, model_load_lock, inference_lock);
                let _ = state_sender.send(ActorState::Ready);

                while let Some(command) = receiver.blocking_recv() {
                    match command {
                        ActorCommand::Call {
                            request,
                            deadline,
                            command_permit,
                            call_state,
                            reply,
                        } => {
                            let is_memory_request =
                                matches!(request.as_ref(), ProjectRequest::Memory(_));
                            let is_optimize_request = matches!(
                                request.as_ref(),
                                ProjectRequest::Memory(request)
                                    if request.method
                                        == crate::memory_proto::Method::Optimize as i32
                            );
                            let initializes_engine = matches!(
                                request.as_ref(),
                                ProjectRequest::Memory(_) | ProjectRequest::Graph(_)
                            );
                            actor_for_thread
                                .active_commands
                                .fetch_add(1, Ordering::AcqRel);
                            drop(command_permit);
                            actor_for_thread.touch();
                            if call_state.load(Ordering::Acquire) != CALL_QUEUED {
                                let _ = reply.send(Err(
                                    "project call was cancelled before transaction start"
                                        .to_string(),
                                ));
                                actor_for_thread.touch();
                                actor_for_thread
                                    .active_commands
                                    .fetch_sub(1, Ordering::AcqRel);
                                continue;
                            }
                            if Instant::now() >= deadline {
                                call_state.store(CALL_CANCELLED, Ordering::Release);
                                let _ =
                                    reply.send(Err("project call deadline exceeded".to_string()));
                                actor_for_thread.touch();
                                actor_for_thread
                                    .active_commands
                                    .fetch_sub(1, Ordering::AcqRel);
                                continue;
                            }
                            if let Err(error) = service.prepare_project_request(&request) {
                                let result = if call_state
                                    .compare_exchange(
                                        CALL_QUEUED,
                                        CALL_COMPLETED,
                                        Ordering::AcqRel,
                                        Ordering::Acquire,
                                    )
                                    .is_ok()
                                {
                                    Ok(crate::rpc::Service::setup_failure_response(
                                        &request, &error,
                                    ))
                                } else {
                                    Err("project call was cancelled before transaction start"
                                        .to_string())
                                };
                                let _ = reply.send(result);
                                actor_for_thread.touch();
                                actor_for_thread
                                    .active_commands
                                    .fetch_sub(1, Ordering::AcqRel);
                                continue;
                            }
                            if Instant::now() >= deadline {
                                call_state.store(CALL_CANCELLED, Ordering::Release);
                                let _ =
                                    reply.send(Err("project call deadline exceeded".to_string()));
                                actor_for_thread.touch();
                                actor_for_thread
                                    .active_commands
                                    .fetch_sub(1, Ordering::AcqRel);
                                continue;
                            }
                            if call_state
                                .compare_exchange(
                                    CALL_QUEUED,
                                    CALL_RUNNING,
                                    Ordering::AcqRel,
                                    Ordering::Acquire,
                                )
                                .is_err()
                            {
                                let _ = reply.send(Err(
                                    "project call was cancelled before transaction start"
                                        .to_string(),
                                ));
                                actor_for_thread.touch();
                                actor_for_thread
                                    .active_commands
                                    .fetch_sub(1, Ordering::AcqRel);
                                continue;
                            }
                            let handled = catch_unwind(AssertUnwindSafe(|| {
                                service.handle_project(*request)
                            }));
                            if initializes_engine && matches!(&handled, Ok(Ok(_))) {
                                let was_initialized = actor_for_thread
                                    .engine_initialized
                                    .swap(true, Ordering::AcqRel);
                                if !was_initialized {
                                    *actor_for_thread
                                        .last_maintenance_attempt
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                        Some(Instant::now());
                                }
                            }
                            if is_optimize_request {
                                *actor_for_thread
                                    .last_maintenance_attempt
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                    Some(Instant::now());
                            }
                            actor_for_thread.touch();
                            actor_for_thread
                                .active_commands
                                .fetch_sub(1, Ordering::AcqRel);
                            call_state.store(CALL_COMPLETED, Ordering::Release);
                            match handled {
                                Ok(result) => {
                                    let result = result
                                        .and_then(|(response, shutdown)| {
                                            if shutdown {
                                                anyhow::ensure!(
                                                    is_memory_request,
                                                    "only memory requests can request shutdown"
                                                );
                                                anyhow::bail!(
                                                    "project actor cannot shut down the daemon"
                                                );
                                            }
                                            Ok(response)
                                        })
                                        .map_err(|error| format!("{error:#}"));
                                    let _ = reply.send(result);
                                }
                                Err(_) => {
                                    let message =
                                        "project actor panicked while executing a project operation";
                                    let _ = reply.send(Err(message.to_string()));
                                    actor_for_thread.record_failure(message.to_string());
                                    let _ =
                                        state_sender.send(ActorState::Failed(message.to_string()));
                                    break;
                                }
                            }
                        }
                        ActorCommand::Maintenance { permit } => {
                            actor_for_thread
                                .active_commands
                                .fetch_add(1, Ordering::AcqRel);
                            let should_run = actor_for_thread.lease_count() > 0
                                && actor_for_thread.engine_initialized.load(Ordering::Acquire)
                                && !actor_for_thread.is_draining();
                            let handled = should_run.then(|| {
                                *actor_for_thread
                                    .last_maintenance_attempt
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                    Some(Instant::now());
                                catch_unwind(AssertUnwindSafe(|| {
                                    service.run_maintenance().map(|_| ())
                                }))
                            });
                            actor_for_thread
                                .active_commands
                                .fetch_sub(1, Ordering::AcqRel);
                            drop(permit);
                            match handled {
                                Some(Ok(Err(error))) => {
                                    eprintln!(
                                        "native memory project maintenance failed for {}: {error:#}",
                                        actor_for_thread.store_key.display()
                                    );
                                }
                                Some(Err(_)) => {
                                    let message = "project actor panicked while executing maintenance";
                                    actor_for_thread.record_failure(message.to_string());
                                    let _ =
                                        state_sender.send(ActorState::Failed(message.to_string()));
                                    break;
                                }
                                Some(Ok(Ok(()))) | None => {}
                            }
                        }
                        ActorCommand::Stop => break,
                    }
                }
                drop(service);
            })
            .map_err(|error| anyhow!("cannot spawn project actor thread: {error}"))?;
        *actor
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(worker);
        Ok(actor)
    }

    pub(crate) fn store_key(&self) -> &Path {
        &self.store_key
    }

    pub(crate) fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub(crate) fn acquire_lease(&self) {
        self.leases.fetch_add(1, Ordering::AcqRel);
        self.touch();
    }

    pub(crate) fn release_lease(&self) {
        let previous = self.leases.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "project actor lease count underflow");
        self.touch();
    }

    pub(crate) fn lease_count(&self) -> usize {
        self.leases.load(Ordering::Acquire)
    }

    pub(crate) fn has_work(&self) -> bool {
        self.pending_commands.load(Ordering::Acquire) > 0
            || self.active_commands.load(Ordering::Acquire) > 0
    }

    pub(crate) fn enqueue_maintenance(&self, interval: Duration) -> bool {
        if self.lease_count() == 0
            || !self.engine_initialized.load(Ordering::Acquire)
            || self.has_work()
        {
            return false;
        }
        let accepting = self
            .accepting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*accepting
            || self.lease_count() == 0
            || !self.engine_initialized.load(Ordering::Acquire)
            || self.has_work()
            || self
                .last_maintenance_attempt
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some_and(|last| last.elapsed() < interval)
            || self
                .maintenance_queued
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return false;
        }

        self.pending_commands.fetch_add(1, Ordering::AcqRel);
        self.sender
            .try_send(ActorCommand::Maintenance {
                permit: MaintenancePermit {
                    pending_commands: Arc::clone(&self.pending_commands),
                    maintenance_queued: Arc::clone(&self.maintenance_queued),
                },
            })
            .is_ok()
    }

    pub(crate) fn is_draining(&self) -> bool {
        matches!(
            *self.state.borrow(),
            ActorState::Draining | ActorState::Failed(_) | ActorState::Closed
        )
    }

    pub(crate) fn begin_draining(&self) {
        let mut accepting = self
            .accepting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*accepting {
            return;
        }
        *accepting = false;
        if matches!(
            *self.state.borrow(),
            ActorState::Failed(_) | ActorState::Closed
        ) {
            return;
        }
        let _ = self.state_sender.send(ActorState::Draining);
    }

    pub(crate) fn is_idle_for(&self, duration: Duration) -> bool {
        self.lease_count() == 0
            && !self.has_work()
            && self.active_commands.load(Ordering::Acquire) == 0
            && self
                .last_activity
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .elapsed()
                >= duration
    }

    pub(crate) async fn wait_ready(&self) -> Result<()> {
        let mut state = self.state.clone();
        loop {
            let current = state.borrow().clone();
            match current {
                ActorState::Opening => state
                    .changed()
                    .await
                    .map_err(|_| anyhow!("project actor stopped while opening"))?,
                ActorState::Ready => return Ok(()),
                ActorState::Draining => return Err(anyhow!("project actor is draining")),
                ActorState::Failed(message) => return Err(anyhow!(message)),
                ActorState::Closed => {
                    let failure = self
                        .failure
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    return Err(anyhow!(
                        failure.unwrap_or_else(|| "project actor is closed".to_string())
                    ));
                }
            }
        }
    }

    pub(crate) async fn call(
        &self,
        request: ProjectRequest,
        deadline: Instant,
        call_state: Arc<AtomicU8>,
    ) -> Result<ProjectResponse> {
        if let Err(error) = self.wait_ready().await {
            call_state.store(CALL_COMPLETED, Ordering::Release);
            return Err(error);
        }
        if Instant::now() >= deadline {
            call_state.store(CALL_COMPLETED, Ordering::Release);
            return Err(anyhow!("project call deadline exceeded"));
        }
        let (reply, response) = oneshot::channel();
        let encoded_len = match &request {
            ProjectRequest::Memory(request) => request.encoded_len(),
            ProjectRequest::Model(request) => request.encoded_len(),
            ProjectRequest::Graph(request) => request.encoded_len(),
        };
        let queue_permit = match QueuePermit::reserve(
            Arc::clone(&self.queued_bytes),
            Arc::clone(&self.global_queued_bytes),
            encoded_len,
            self.global_queue_limit,
        ) {
            Ok(permit) => permit,
            Err(error) => {
                call_state.store(CALL_COMPLETED, Ordering::Release);
                return Err(error);
            }
        };
        let result = {
            let accepting = self
                .accepting
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !*accepting {
                call_state.store(CALL_COMPLETED, Ordering::Release);
                return Err(anyhow!("project actor is draining"));
            }
            self.pending_commands.fetch_add(1, Ordering::AcqRel);
            let command = ActorCommand::Call {
                request: Box::new(request),
                deadline,
                command_permit: CommandPermit {
                    _queue: queue_permit,
                    pending_commands: Arc::clone(&self.pending_commands),
                },
                call_state: Arc::clone(&call_state),
                reply,
            };
            self.sender.try_send(command)
        };
        if let Err(error) = result {
            call_state.store(CALL_COMPLETED, Ordering::Release);
            return Err(match error {
                mpsc::error::TrySendError::Full(_) => anyhow!("project command queue is full"),
                mpsc::error::TrySendError::Closed(_) => anyhow!("project actor is unavailable"),
            });
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        tokio::time::timeout(remaining, response)
            .await
            .map_err(|_| anyhow!("project call deadline exceeded"))?
            .map_err(|_| anyhow!("project actor dropped the response"))?
            .map_err(anyhow::Error::msg)
    }

    pub(crate) async fn stop(&self) {
        self.begin_draining();
        let _ = self.sender.send(ActorCommand::Stop).await;
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(worker) = worker {
            let _ = tokio::task::spawn_blocking(move || worker.join()).await;
        }
    }

    pub(crate) async fn wait_closed(&self) {
        let mut state = self.state.clone();
        loop {
            if matches!(*state.borrow(), ActorState::Closed) {
                return;
            }
            if state.changed().await.is_err() {
                return;
            }
        }
    }

    fn touch(&self) {
        *self
            .last_activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
    }

    fn record_failure(&self, message: String) {
        *self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance_is_actor_owned_and_coalesced() {
        let (sender, mut receiver) = mpsc::channel(PROJECT_QUEUE_CAPACITY);
        let (state_sender, state) = watch::channel(ActorState::Ready);
        let actor = ProjectActor {
            store_key: PathBuf::from("/tmp/maintenance-test"),
            fingerprint: "maintenance-test".to_string(),
            sender,
            state,
            state_sender,
            accepting: Mutex::new(true),
            leases: AtomicUsize::new(1),
            active_commands: AtomicUsize::new(0),
            pending_commands: Arc::new(AtomicUsize::new(0)),
            engine_initialized: AtomicBool::new(true),
            maintenance_queued: Arc::new(AtomicBool::new(false)),
            last_maintenance_attempt: Mutex::new(None),
            failure: Mutex::new(None),
            queued_bytes: Arc::new(AtomicUsize::new(0)),
            global_queued_bytes: Arc::new(AtomicUsize::new(0)),
            global_queue_limit: AGGREGATE_TEST_QUEUE_BYTES,
            last_activity: Mutex::new(Instant::now()),
            worker: Mutex::new(None),
        };

        assert!(actor.enqueue_maintenance(Duration::ZERO));
        assert!(!actor.enqueue_maintenance(Duration::ZERO));
        assert_eq!(actor.pending_commands.load(Ordering::Acquire), 1);

        let command = receiver.try_recv().expect("maintenance command");
        assert!(matches!(&command, ActorCommand::Maintenance { .. }));
        drop(command);
        assert_eq!(actor.pending_commands.load(Ordering::Acquire), 0);
        assert!(!actor.maintenance_queued.load(Ordering::Acquire));
        *actor
            .last_maintenance_attempt
            .lock()
            .expect("maintenance timestamp lock") = Some(Instant::now());
        assert!(!actor.enqueue_maintenance(Duration::from_secs(60)));
    }

    #[test]
    fn maintenance_skips_inactive_or_lazy_actors() {
        let (sender, mut receiver) = mpsc::channel(PROJECT_QUEUE_CAPACITY);
        let (state_sender, state) = watch::channel(ActorState::Ready);
        let actor = ProjectActor {
            store_key: PathBuf::from("/tmp/maintenance-test"),
            fingerprint: "maintenance-test".to_string(),
            sender,
            state,
            state_sender,
            accepting: Mutex::new(true),
            leases: AtomicUsize::new(0),
            active_commands: AtomicUsize::new(0),
            pending_commands: Arc::new(AtomicUsize::new(0)),
            engine_initialized: AtomicBool::new(false),
            maintenance_queued: Arc::new(AtomicBool::new(false)),
            last_maintenance_attempt: Mutex::new(None),
            failure: Mutex::new(None),
            queued_bytes: Arc::new(AtomicUsize::new(0)),
            global_queued_bytes: Arc::new(AtomicUsize::new(0)),
            global_queue_limit: AGGREGATE_TEST_QUEUE_BYTES,
            last_activity: Mutex::new(Instant::now()),
            worker: Mutex::new(None),
        };

        actor.engine_initialized.store(true, Ordering::Release);
        assert!(!actor.enqueue_maintenance(Duration::ZERO));
        actor.engine_initialized.store(false, Ordering::Release);
        actor.leases.store(1, Ordering::Release);
        assert!(!actor.enqueue_maintenance(Duration::ZERO));
        assert!(receiver.try_recv().is_err());
    }

    const AGGREGATE_TEST_QUEUE_BYTES: usize = PROJECT_QUEUE_BYTES * 2;
}
