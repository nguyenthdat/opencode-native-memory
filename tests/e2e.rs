use std::fs;

use opencode_memory::{
    FeedbackEvent, FeedbackRequest, ForgetRequest, GetRequest, ListRequest, LockAction,
    LockRequest, MemoryConfig, MemoryEngine, MemoryKind, MemoryOrigin, MemoryScope, MemoryTaxonomy,
    PinRequest, SearchRequest, StoreRequest, UpdateRequest,
};

#[test]
#[ignore = "downloads the multilingual embedding model on first run"]
#[allow(clippy::too_many_lines)]
fn stores_recalls_and_forgets_project_memory() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let project = temp.path().join("project");
    fs::create_dir_all(&project).expect("create project dir");
    let config = MemoryConfig::new(project, temp.path().join("data"), model_cache());
    let mut engine = MemoryEngine::open(config).expect("open native memory");

    let rust = engine
        .store(StoreRequest {
            content: "Quyết định dùng Rust và zvec cho bộ nhớ native của OpenCode.".to_string(),
            title: Some("Kiến trúc memory".to_string()),
            kind: MemoryKind::Decision,
            importance: 0.9,
            tags: vec!["rust".to_string(), "zvec".to_string()],
            source: Some("e2e-test".to_string()),
            scope: MemoryScope::Project,
            scope_key: None,
            origin: MemoryOrigin::Manual,
            expires_in_days: None,
            code_paths: Vec::new(),
            revive: false,
            taxonomy: None,
            confidence: None,
        })
        .expect("store Rust decision");
    engine
        .store(StoreRequest {
            content: "Giao diện người dùng ưu tiên màu xanh lá.".to_string(),
            title: Some("Màu giao diện".to_string()),
            kind: MemoryKind::Preference,
            importance: 0.4,
            tags: vec!["ui".to_string()],
            source: Some("e2e-test".to_string()),
            scope: MemoryScope::Project,
            scope_key: None,
            origin: MemoryOrigin::Manual,
            expires_in_days: None,
            code_paths: Vec::new(),
            revive: false,
            taxonomy: None,
            confidence: None,
        })
        .expect("store unrelated preference");
    engine
        .store(StoreRequest {
            content:
                "Rust sidecar kết hợp zvec dense search và full-text search cho OpenCode memory."
                    .to_string(),
            title: Some("Hybrid memory retrieval".to_string()),
            kind: MemoryKind::Pattern,
            importance: 0.7,
            tags: vec!["rust".to_string(), "zvec".to_string()],
            source: Some("e2e-test".to_string()),
            scope: MemoryScope::Project,
            scope_key: None,
            origin: MemoryOrigin::Manual,
            expires_in_days: None,
            code_paths: Vec::new(),
            revive: false,
            taxonomy: None,
            confidence: None,
        })
        .expect("store related retrieval pattern");

    let results = engine
        .search(&SearchRequest {
            query: "Memory server được viết bằng ngôn ngữ và database nào?".to_string(),
            limit: Some(2),
            max_results: 20,
            budget_chars: 6_000,
            kinds: Vec::new(),
            scopes: Vec::new(),
            taxonomies: Vec::new(),
            session_scope_key: None,
            agent_scope_key: None,
            min_score: 0.0,
            include_stale: false,
            include_superseded: false,
            track_feedback: true,
        })
        .expect("search memories");
    assert_eq!(
        results.memories.first().map(|memory| memory.id.as_str()),
        Some(rust.id.as_str())
    );

    let fetched = engine
        .get(&GetRequest {
            ids: vec![rust.id.clone()],
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("fetch memory");
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].kind, MemoryKind::Decision);

    let feedback_search = engine
        .search(&SearchRequest {
            query: "OpenCode memory Rust zvec search".to_string(),
            limit: Some(5),
            max_results: 20,
            budget_chars: 6_000,
            kinds: Vec::new(),
            scopes: Vec::new(),
            taxonomies: Vec::new(),
            session_scope_key: None,
            agent_scope_key: None,
            min_score: 0.0,
            include_stale: false,
            include_superseded: false,
            track_feedback: true,
        })
        .expect("search feedback candidates");
    assert!(feedback_search.memories.len() >= 2);
    let retrieval_id = feedback_search
        .retrieval_id
        .expect("tracked search has retrieval id");
    let first_id = feedback_search.memories[0].id.clone();
    let second_id = feedback_search.memories[1].id.clone();
    let first_feedback = engine
        .feedback(&FeedbackRequest {
            retrieval_id: retrieval_id.clone(),
            event: FeedbackEvent::Injected,
            memory_ids: vec![first_id.clone()],
        })
        .expect("record first feedback subset");
    let second_feedback = engine
        .feedback(&FeedbackRequest {
            retrieval_id: retrieval_id.clone(),
            event: FeedbackEvent::Injected,
            memory_ids: vec![second_id.clone()],
        })
        .expect("record second feedback subset");
    assert_eq!(first_feedback.affected, 1);
    assert_eq!(second_feedback.affected, 1);
    let duplicate_feedback = engine
        .feedback(&FeedbackRequest {
            retrieval_id,
            event: FeedbackEvent::Injected,
            memory_ids: vec![first_id],
        })
        .expect("deduplicate repeated feedback subset");
    assert!(!duplicate_feedback.recorded);

    let session_memory = engine
        .store(StoreRequest {
            content: "Session family A private coordination context.".to_string(),
            title: Some("Private session context".to_string()),
            kind: MemoryKind::Fact,
            importance: 0.5,
            tags: vec!["session".to_string()],
            source: Some("e2e-test".to_string()),
            scope: MemoryScope::Session,
            scope_key: Some("family-a".to_string()),
            origin: MemoryOrigin::Manual,
            expires_in_days: None,
            code_paths: Vec::new(),
            revive: false,
            taxonomy: None,
            confidence: None,
        })
        .expect("store session memory");
    let hidden = engine
        .get(&GetRequest {
            ids: vec![session_memory.id.clone()],
            session_scope_key: Some("family-b".to_string()),
            agent_scope_key: None,
        })
        .expect("fetch from unrelated session");
    assert!(hidden.is_empty());
    let visible = engine
        .list(&ListRequest {
            kinds: Vec::new(),
            scopes: vec![MemoryScope::Session],
            taxonomies: Vec::new(),
            include_expired: false,
            include_stale: false,
            include_superseded: false,
            offset: 0,
            limit: 50,
            session_scope_key: Some("family-a".to_string()),
            agent_scope_key: None,
        })
        .expect("list matching session family");
    assert!(
        visible
            .memories
            .iter()
            .any(|memory| memory.id == session_memory.id)
    );

    let forgotten = engine
        .forget(&ForgetRequest {
            ids: vec![rust.id],
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("forget memory");
    assert_eq!(forgotten.deleted, 1);
}

fn model_cache() -> std::path::PathBuf {
    std::env::var_os("OPENCODE_MEMORY_MODEL_CACHE").map_or_else(
        || {
            std::env::var_os("HOME").map_or_else(
                || std::path::PathBuf::from(".cache/opencode/memory/models"),
                |home| std::path::PathBuf::from(home).join(".cache/opencode/memory/models"),
            )
        },
        std::path::PathBuf::from,
    )
}

fn make_engine() -> (tempfile::TempDir, MemoryEngine) {
    let temp = tempfile::tempdir().expect("create temp dir");
    let project = temp.path().join("project");
    fs::create_dir_all(&project).expect("create project dir");
    let config = MemoryConfig::new(project, temp.path().join("data"), model_cache());
    let engine = MemoryEngine::open(config).expect("open native memory");
    (temp, engine)
}

fn store_decision(engine: &mut MemoryEngine, content: &str) -> String {
    engine
        .store(StoreRequest {
            content: content.to_string(),
            title: None,
            kind: MemoryKind::Decision,
            importance: 0.8,
            tags: Vec::new(),
            source: Some("test".to_string()),
            scope: MemoryScope::Project,
            scope_key: None,
            origin: MemoryOrigin::Manual,
            expires_in_days: None,
            code_paths: Vec::new(),
            revive: false,
            taxonomy: None,
            confidence: None,
        })
        .expect("store decision")
        .id
}

#[test]
#[ignore = "downloads the multilingual embedding model on first run"]
fn pin_lock_parity_with_update() {
    let (_temp, mut engine) = make_engine();
    let id = store_decision(&mut engine, "Use Rust for memory sidecar.");

    // Pin via dedicated RPC.
    let pin_response = engine
        .pin(&PinRequest {
            id: id.clone(),
            pinned: true,
            expected_updated_at_ms: None,
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("pin memory");
    assert!(pin_response.pinned);
    assert_eq!(pin_response.id, id);

    // Verify via get that pinned is set.
    let fetched = engine
        .get(&GetRequest {
            ids: vec![id.clone()],
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("fetch pinned memory");
    assert!(fetched[0].pinned);

    // Unpin via dedicated RPC.
    let unpin = engine
        .pin(&PinRequest {
            id: id.clone(),
            pinned: false,
            expected_updated_at_ms: None,
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("unpin memory");
    assert!(!unpin.pinned);

    // Lock via dedicated RPC.
    let lock_response = engine
        .lock(&LockRequest {
            id: id.clone(),
            lock_action: LockAction::Lock,
            lock_reason: Some("curated".to_string()),
            expected_updated_at_ms: None,
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("lock memory");
    assert!(lock_response.locked);
    assert_eq!(lock_response.lock_reason.as_deref(), Some("curated"));

    // Verify locked memory rejects content updates.
    let locked_update = engine.update(UpdateRequest {
        id: id.clone(),
        expected_updated_at_ms: None,
        content: Some("changed".to_string()),
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
    });
    assert!(locked_update.is_err());

    // Unlock via dedicated RPC.
    let unlock = engine
        .lock(&LockRequest {
            id: id.clone(),
            lock_action: LockAction::Unlock,
            lock_reason: None,
            expected_updated_at_ms: None,
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("unlock memory");
    assert!(!unlock.locked);
}

#[test]
#[ignore = "downloads the multilingual embedding model on first run"]
fn supersession_chain_keeps_predecessor_and_excludes_by_default() {
    let (_temp, mut engine) = make_engine();
    let id_a = store_decision(&mut engine, "Original architecture decision A.");

    // Update A -> B (identity change via content change).
    let update_b = engine
        .update(UpdateRequest {
            id: id_a.clone(),
            expected_updated_at_ms: None,
            content: Some("Revised architecture decision B.".to_string()),
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
        })
        .expect("update A to B");
    let id_b = update_b.id;
    assert_ne!(id_a, id_b);
    assert_eq!(update_b.previous_id, Some(id_a.clone()));

    // A still exists (get by old ID works).
    let fetched_a = engine
        .get(&GetRequest {
            ids: vec![id_a.clone()],
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("fetch A");
    assert_eq!(fetched_a.len(), 1);
    assert_eq!(fetched_a[0].superseded_by.as_deref(), Some(id_b.as_str()));

    // B exists and supersedes A.
    let fetched_b = engine
        .get(&GetRequest {
            ids: vec![id_b.clone()],
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("fetch B");
    assert_eq!(fetched_b.len(), 1);
    assert!(fetched_b[0].supersedes.contains(&id_a));

    // Default list excludes A (superseded).
    let listed = engine
        .list(&ListRequest {
            kinds: Vec::new(),
            scopes: Vec::new(),
            taxonomies: Vec::new(),
            include_expired: false,
            include_stale: false,
            include_superseded: false,
            offset: 0,
            limit: 50,
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("list without superseded");
    assert!(!listed.memories.iter().any(|m| m.id == id_a));

    // include_superseded=true shows A.
    let listed_all = engine
        .list(&ListRequest {
            kinds: Vec::new(),
            scopes: Vec::new(),
            taxonomies: Vec::new(),
            include_expired: false,
            include_stale: false,
            include_superseded: true,
            offset: 0,
            limit: 50,
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("list with superseded");
    assert!(listed_all.memories.iter().any(|m| m.id == id_a));

    // Update B -> C (chain A -> B -> C).
    let update_c = engine
        .update(UpdateRequest {
            id: id_b.clone(),
            expected_updated_at_ms: None,
            content: Some("Final architecture decision C.".to_string()),
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
        })
        .expect("update B to C");
    let id_c = update_c.id;
    assert_ne!(id_b, id_c);

    // B is now superseded by C.
    let fetched_b_after = engine
        .get(&GetRequest {
            ids: vec![id_b.clone()],
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("fetch B after chain");
    assert_eq!(
        fetched_b_after[0].superseded_by.as_deref(),
        Some(id_c.as_str())
    );

    // C supersedes B.
    let fetched_c = engine
        .get(&GetRequest {
            ids: vec![id_c.clone()],
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("fetch C");
    assert!(fetched_c[0].supersedes.contains(&id_b));
}

#[test]
#[ignore = "downloads the multilingual embedding model on first run"]
fn symmetric_conflict_add_clear_and_lock_enforcement() {
    let (_temp, mut engine) = make_engine();
    let id_a = store_decision(&mut engine, "Decision A: use Rust.");
    let id_b = store_decision(&mut engine, "Decision B: use Go.");

    // Add conflict A <-> B.
    engine
        .update(UpdateRequest {
            id: id_a.clone(),
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
            conflict_with: Some(vec![id_b.clone()]),
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("add conflict");

    // Verify symmetric link.
    let fetched_a = engine
        .get(&GetRequest {
            ids: vec![id_a.clone()],
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("fetch A");
    let fetched_b = engine
        .get(&GetRequest {
            ids: vec![id_b.clone()],
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("fetch B");
    assert!(fetched_a[0].conflict_with.contains(&id_b));
    assert!(fetched_b[0].conflict_with.contains(&id_a));
    // Confidence capped at 0.5 for both.
    assert!(fetched_a[0].confidence <= 0.5);
    assert!(fetched_b[0].confidence <= 0.5);

    // Clear conflict (empty list) — links removed, confidence not restored.
    engine
        .update(UpdateRequest {
            id: id_a.clone(),
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
            conflict_with: Some(Vec::new()),
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("clear conflict");

    let fetched_a_after = engine
        .get(&GetRequest {
            ids: vec![id_a.clone()],
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("fetch A after clear");
    let fetched_b_cleared = engine
        .get(&GetRequest {
            ids: vec![id_b.clone()],
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("fetch B after clear");
    assert!(fetched_a_after[0].conflict_with.is_empty());
    assert!(fetched_b_cleared[0].conflict_with.is_empty());
    // Confidence still capped (not restored).
    assert!(fetched_a_after[0].confidence <= 0.5);
    assert!(fetched_b_cleared[0].confidence <= 0.5);

    // Lock A, then conflict update should be rejected.
    engine
        .lock(&LockRequest {
            id: id_a.clone(),
            lock_action: LockAction::Lock,
            lock_reason: Some("locked".to_string()),
            expected_updated_at_ms: None,
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("lock A");
    let locked_conflict = engine.update(UpdateRequest {
        id: id_a.clone(),
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
        conflict_with: Some(vec![id_b.clone()]),
        session_scope_key: None,
        agent_scope_key: None,
    });
    assert!(locked_conflict.is_err());
}

#[test]
#[ignore = "downloads the multilingual embedding model on first run"]
fn taxonomy_filter_in_search_and_list() {
    let (_temp, mut engine) = make_engine();
    store_decision(&mut engine, "Decide to use Rust for memory.");
    engine
        .store(StoreRequest {
            content: "Prefer dark mode UI.".to_string(),
            title: None,
            kind: MemoryKind::Preference,
            importance: 0.5,
            tags: Vec::new(),
            source: Some("test".to_string()),
            scope: MemoryScope::Project,
            scope_key: None,
            origin: MemoryOrigin::Manual,
            expires_in_days: None,
            code_paths: Vec::new(),
            revive: false,
            taxonomy: None,
            confidence: None,
        })
        .expect("store preference");

    // List with taxonomy filter for Decision only.
    let decisions = engine
        .list(&ListRequest {
            kinds: Vec::new(),
            scopes: Vec::new(),
            taxonomies: vec![MemoryTaxonomy::Decision],
            include_expired: false,
            include_stale: false,
            include_superseded: false,
            offset: 0,
            limit: 50,
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("list decisions");
    assert!(
        decisions
            .memories
            .iter()
            .all(|m| m.taxonomy == MemoryTaxonomy::Decision)
    );

    // List with taxonomy filter for WorkflowPref only.
    let prefs = engine
        .list(&ListRequest {
            kinds: Vec::new(),
            scopes: Vec::new(),
            taxonomies: vec![MemoryTaxonomy::WorkflowPref],
            include_expired: false,
            include_stale: false,
            include_superseded: false,
            offset: 0,
            limit: 50,
            session_scope_key: None,
            agent_scope_key: None,
        })
        .expect("list prefs");
    assert!(
        prefs
            .memories
            .iter()
            .all(|m| m.taxonomy == MemoryTaxonomy::WorkflowPref)
    );
}
