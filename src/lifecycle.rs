//! Lifecycle policy for expiry, retention, pinning, and locking.

use anyhow::{Result, ensure};

use crate::contract::{LockAction, MemoryKind, MemoryScope, UpdateRequest};
use crate::storage::state::MemoryMetadata;

const MILLIS_PER_DAY: i64 = 86_400_000;
const MAX_LOCK_REASON_CHARS: usize = 240;

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct LifecycleValues {
    pub(crate) pinned: bool,
    pub(crate) locked: bool,
    pub(crate) lock_reason: Option<String>,
}

pub(crate) fn default_half_life_days(kind: MemoryKind) -> f32 {
    match kind {
        MemoryKind::Decision => 730.0,
        MemoryKind::Preference | MemoryKind::Gotcha => 365.0,
        MemoryKind::Fact => 180.0,
        MemoryKind::Pattern => 270.0,
        MemoryKind::Summary => 14.0,
    }
}

pub(crate) fn default_expiry(kind: MemoryKind, created_at_ms: i64) -> Option<i64> {
    let days = match kind {
        MemoryKind::Decision | MemoryKind::Preference => return None,
        MemoryKind::Fact => 365,
        MemoryKind::Pattern => 540,
        MemoryKind::Gotcha => 730,
        MemoryKind::Summary => 30,
    };
    Some(created_at_ms.saturating_add(days * MILLIS_PER_DAY))
}

pub(crate) fn expiry_from_days(now_ms: i64, days: Option<u32>) -> Option<i64> {
    days.map(|days| now_ms.saturating_add(i64::from(days) * MILLIS_PER_DAY))
}

pub(crate) fn is_expired(metadata: &MemoryMetadata, now_ms: i64) -> bool {
    !metadata.pinned
        && metadata
            .expires_at_ms
            .is_some_and(|expires_at_ms| expires_at_ms <= now_ms)
}

pub(crate) fn is_prunable_expired(metadata: &MemoryMetadata, now_ms: i64) -> bool {
    !metadata.locked && is_expired(metadata, now_ms)
}

pub(crate) fn retention_factor(now_ms: i64, updated_at_ms: i64, metadata: &MemoryMetadata) -> f32 {
    if metadata.pinned {
        return 1.0;
    }
    let age_days = now_ms.saturating_sub(updated_at_ms).max(0) / MILLIS_PER_DAY;
    let bounded_age = u16::try_from(age_days.min(i64::from(u16::MAX))).unwrap_or(u16::MAX);
    2.0_f32.powf(-f32::from(bounded_age) / metadata.half_life_days.max(1.0))
}

pub(crate) fn resolve_update(
    metadata: &MemoryMetadata,
    request: &UpdateRequest,
    target_scope: MemoryScope,
) -> Result<LifecycleValues> {
    ensure!(
        request.lock_reason.is_none() || request.lock_action == Some(LockAction::Lock),
        "lock_reason is valid only with lock_action=lock"
    );
    if request.lock_action == Some(LockAction::Unlock) {
        ensure!(
            !has_record_mutation(request),
            "unlock must be a lifecycle-only update"
        );
    }
    if metadata.locked && request.lock_action != Some(LockAction::Unlock) {
        ensure!(
            !has_locked_forbidden_mutation(request),
            "locked memory rejects content, scope, kind, expiry, code-anchor, and pin updates"
        );
    }

    let mut values = LifecycleValues {
        pinned: request.pinned.unwrap_or(metadata.pinned),
        locked: metadata.locked,
        lock_reason: metadata.lock_reason.clone(),
    };
    match request.lock_action {
        Some(LockAction::Lock) => {
            values.locked = true;
            if let Some(reason) = request.lock_reason.as_deref() {
                values.lock_reason = normalize_lock_reason(reason)?;
            }
        }
        Some(LockAction::Unlock) => {
            values.locked = false;
            values.lock_reason = None;
        }
        None => {}
    }
    ensure!(
        target_scope != MemoryScope::Repository || (!values.pinned && !values.locked),
        "repository memory cannot be pinned or locked via RPC"
    );
    Ok(values)
}

pub(crate) fn ensure_store_overwrite_allowed(metadata: &MemoryMetadata) -> Result<()> {
    ensure!(
        !metadata.locked,
        "locked memory cannot be overwritten by store"
    );
    Ok(())
}

pub(crate) fn ensure_delete_allowed(metadata: &MemoryMetadata) -> Result<()> {
    ensure!(
        !metadata.locked,
        "locked memory cannot be deleted or forgotten"
    );
    Ok(())
}

fn normalize_lock_reason(reason: &str) -> Result<Option<String>> {
    let reason = reason.trim();
    ensure!(
        reason.chars().count() <= MAX_LOCK_REASON_CHARS,
        "lock_reason exceeds {MAX_LOCK_REASON_CHARS} characters"
    );
    ensure!(
        !reason.contains('\0'),
        "lock_reason cannot contain NUL bytes"
    );
    crate::validation::scan_sensitive("lock_reason", reason)?;
    Ok((!reason.is_empty()).then(|| reason.to_string()))
}

fn has_record_mutation(request: &UpdateRequest) -> bool {
    request.content.is_some()
        || request.title.is_some()
        || request.kind.is_some()
        || request.importance.is_some()
        || request.tags.is_some()
        || request.scope.is_some()
        || request.scope_key.is_some()
        || request.expires_in_days.is_some()
        || request.clear_expiry
        || request.code_paths.is_some()
        || request.pinned.is_some()
}

fn has_locked_forbidden_mutation(request: &UpdateRequest) -> bool {
    request.content.is_some()
        || request.kind.is_some()
        || request.scope.is_some()
        || request.scope_key.is_some()
        || request.expires_in_days.is_some()
        || request.clear_expiry
        || request.code_paths.is_some()
        || request.pinned.is_some()
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_delete_allowed, ensure_store_overwrite_allowed, is_expired, is_prunable_expired,
        resolve_update, retention_factor,
    };
    use crate::contract::{FeedbackStats, LockAction, MemoryOrigin, MemoryScope, UpdateRequest};
    use crate::storage::state::MemoryMetadata;

    fn metadata() -> MemoryMetadata {
        MemoryMetadata {
            scope: MemoryScope::Project,
            scope_key: None,
            origin: MemoryOrigin::Manual,
            expires_at_ms: Some(10),
            half_life_days: 1.0,
            code_anchors: Vec::new(),
            feedback: FeedbackStats::default(),
            shared_source: None,
            pinned: false,
            locked: false,
            lock_reason: None,
            taxonomy: crate::taxonomy::MemoryTaxonomy::Decision,
            confidence: 0.7,
            superseded_by: None,
            supersedes: Vec::new(),
            conflict_with: Vec::new(),
        }
    }

    fn update() -> UpdateRequest {
        UpdateRequest {
            id: "mem_00000000000000000000000000000000".to_string(),
            expected_updated_at_ms: None,
            content: None,
            title: None,
            kind: None,
            importance: None,
            tags: None,
            scope: None,
            scope_key: None,
            expires_in_days: None,
            clear_expiry: false,
            code_paths: None,
            pinned: None,
            lock_action: None,
            lock_reason: None,
            taxonomy: None,
            confidence: None,
            conflict_with: None,
            session_scope_key: None,
            agent_scope_key: None,
        }
    }

    #[test]
    fn pinned_memory_bypasses_expiry_and_retention_decay() {
        let mut value = metadata();
        assert!(is_expired(&value, 11));
        assert!(retention_factor(86_400_000, 0, &value) < 1.0);

        value.pinned = true;

        assert!(!is_expired(&value, 11));
        assert!((retention_factor(86_400_000, 0, &value) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn validates_lock_reason_and_unlock_lifecycle_only_semantics() {
        let value = metadata();
        let mut invalid_reason = update();
        invalid_reason.lock_reason = Some("reason".to_string());
        assert!(resolve_update(&value, &invalid_reason, value.scope).is_err());

        let mut unlock_and_mutate = update();
        unlock_and_mutate.lock_action = Some(LockAction::Unlock);
        unlock_and_mutate.title = Some("changed".to_string());
        assert!(resolve_update(&value, &unlock_and_mutate, value.scope).is_err());

        let mut long_reason = update();
        long_reason.lock_action = Some(LockAction::Lock);
        long_reason.lock_reason = Some("x".repeat(241));
        assert!(resolve_update(&value, &long_reason, value.scope).is_err());
    }

    #[test]
    fn locked_memory_rejects_protected_mutations_store_delete_and_forget() {
        let mut value = metadata();
        value.locked = true;
        let mut content_update = update();
        content_update.content = Some("changed".to_string());
        assert!(resolve_update(&value, &content_update, value.scope).is_err());

        let mut pin_update = update();
        pin_update.pinned = Some(true);
        assert!(resolve_update(&value, &pin_update, value.scope).is_err());
        assert!(ensure_store_overwrite_allowed(&value).is_err());
        assert!(ensure_delete_allowed(&value).is_err());
    }

    #[test]
    fn optimize_eligibility_excludes_pinned_and_locked_expired_records() {
        let mut value = metadata();
        assert!(is_prunable_expired(&value, 11));

        value.pinned = true;
        assert!(!is_prunable_expired(&value, 11));

        value.pinned = false;
        value.locked = true;
        assert!(!is_prunable_expired(&value, 11));
    }

    #[test]
    fn repository_lifecycle_flags_are_rejected() {
        let value = metadata();
        let mut pin = update();
        pin.pinned = Some(true);
        assert!(resolve_update(&value, &pin, MemoryScope::Repository).is_err());

        let mut lock = update();
        lock.lock_action = Some(LockAction::Lock);
        assert!(resolve_update(&value, &lock, MemoryScope::Repository).is_err());
    }
}
