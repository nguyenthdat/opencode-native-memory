use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};

use super::actor::ProjectActor;
use crate::MemoryConfig;

const DEFAULT_MAX_ACTIVE_ACTORS: usize = 2;
const AGGREGATE_QUEUE_BYTES: usize = 256 * 1024 * 1024;

pub(crate) struct ProjectRegistry {
    actors: Mutex<HashMap<PathBuf, Arc<ProjectActor>>>,
    model_load_lock: Arc<Mutex<()>>,
    inference_lock: Arc<Mutex<()>>,
    max_active_actors: usize,
    queued_bytes: Arc<AtomicUsize>,
}

impl ProjectRegistry {
    pub(crate) fn new() -> Self {
        Self {
            actors: Mutex::new(HashMap::new()),
            model_load_lock: Arc::new(Mutex::new(())),
            inference_lock: Arc::new(Mutex::new(())),
            max_active_actors: DEFAULT_MAX_ACTIVE_ACTORS,
            queued_bytes: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub(crate) async fn acquire(
        &self,
        config: MemoryConfig,
        store_key: PathBuf,
        fingerprint: String,
    ) -> Result<Arc<ProjectActor>> {
        let actor = loop {
            let actor = {
                let mut actors = self
                    .actors
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(actor) = actors.get(&store_key) {
                    if actor.fingerprint() != fingerprint {
                        return Err(anyhow!(
                            "project configuration does not match the daemon actor already owning store {}",
                            store_key.display()
                        ));
                    }
                    if actor.is_draining() {
                        Err(Arc::clone(actor))
                    } else {
                        actor.acquire_lease();
                        Ok(Arc::clone(actor))
                    }
                } else {
                    if actors.len() >= self.max_active_actors {
                        return Err(anyhow!(
                            "daemon active project limit ({}) is exhausted",
                            self.max_active_actors
                        ));
                    }
                    let actor = ProjectActor::spawn(
                        config.clone(),
                        store_key.clone(),
                        fingerprint.clone(),
                        Arc::clone(&self.model_load_lock),
                        Arc::clone(&self.inference_lock),
                        Arc::clone(&self.queued_bytes),
                        AGGREGATE_QUEUE_BYTES,
                    )?;
                    actor.acquire_lease();
                    actors.insert(store_key.clone(), Arc::clone(&actor));
                    Ok(actor)
                }
            };
            match actor {
                Ok(actor) => break actor,
                Err(actor) => {
                    actor.wait_closed().await;
                    self.remove_if_current(&actor);
                }
            }
        };

        if let Err(error) = actor.wait_ready().await {
            actor.release_lease();
            actor.stop().await;
            self.remove_if_current(&actor);
            return Err(error);
        }
        Ok(actor)
    }

    pub(crate) async fn evict_idle(&self, idle_for: Duration) {
        let evicted = {
            let actors = self
                .actors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            actors
                .values()
                .filter(|actor| actor.is_idle_for(idle_for))
                .map(|actor| {
                    actor.begin_draining();
                    Arc::clone(actor)
                })
                .collect::<Vec<_>>()
        };
        for actor in evicted {
            actor.stop().await;
            self.remove_if_current(&actor);
        }
    }

    pub(crate) fn schedule_maintenance(&self, interval: Duration) -> usize {
        let actors = self
            .actors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        actors
            .into_iter()
            .filter(|actor| actor.enqueue_maintenance(interval))
            .count()
    }

    pub(crate) fn has_activity(&self) -> bool {
        self.actors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .any(|actor| actor.lease_count() > 0 || actor.has_work())
    }

    pub(crate) async fn close_all(&self) {
        let actors = {
            let actors = self
                .actors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            actors
                .values()
                .map(|actor| {
                    actor.begin_draining();
                    Arc::clone(actor)
                })
                .collect::<Vec<_>>()
        };
        for actor in actors {
            actor.stop().await;
            self.remove_if_current(&actor);
        }
    }

    fn remove_if_current(&self, actor: &Arc<ProjectActor>) {
        let mut actors = self
            .actors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if actors
            .get(actor.store_key())
            .is_some_and(|current| Arc::ptr_eq(current, actor))
        {
            actors.remove(actor.store_key());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EmbeddingConfig;

    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_alias_acquires_share_one_lazy_actor() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");
        let embedding = EmbeddingConfig {
            model_path: Some(temp.path().join("missing.gguf")),
            ..EmbeddingConfig::default()
        };
        let config =
            MemoryConfig::new(project, temp.path().join("data"), temp.path().join("cache"))
                .with_embedding(embedding);
        let store_key = temp.path().join("canonical-store");
        let registry = Arc::new(ProjectRegistry::new());
        let first_registry = Arc::clone(&registry);
        let first_config = config.clone();
        let first_key = store_key.clone();
        let first = tokio::spawn(async move {
            first_registry
                .acquire(first_config, first_key, "fingerprint-a".to_string())
                .await
        });
        tokio::task::yield_now().await;
        let second_registry = Arc::clone(&registry);
        let second = tokio::spawn(async move {
            second_registry
                .acquire(config, store_key, "fingerprint-a".to_string())
                .await
        });
        tokio::task::yield_now().await;

        {
            let actors = registry
                .actors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(actors.len(), 1);
            assert_eq!(actors.values().next().expect("actor").lease_count(), 2);
        }
        let first = first.await.expect("first task").expect("first actor");
        let second = second.await.expect("second task").expect("second actor");
        assert!(Arc::ptr_eq(&first, &second));
        first.release_lease();
        second.release_lease();
        first.stop().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn periodic_maintenance_does_not_create_an_actor() {
        let registry = ProjectRegistry::new();

        assert_eq!(registry.schedule_maintenance(Duration::ZERO), 0);
        assert!(registry.actors.lock().expect("registry lock").is_empty());
    }
}
