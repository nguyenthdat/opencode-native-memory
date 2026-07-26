import { tool } from "@opencode-ai/plugin";
import { MEMORY_KINDS, MEMORY_SCOPES, WRITABLE_MEMORY_SCOPES, FEEDBACK_EVENTS, LOCK_ACTIONS, MEMORY_TAXONOMIES, RETRIEVAL_MODES, } from "./contracts.js";
import { acquireNativeMemoryClient } from "./sidecar-client.js";
import { COMPACTION_CONTEXT, formatRecalledMemories, truncateText, contextBudgetChars, parseCuratedCandidates, deriveRecallQuery, } from "./policy.js";
import { MEMORY_INSTRUCTIONS_MARKER, loadMemoryInstructions, registerMemoryInstructions, } from "./instructions.js";
import { SHARED_MEMORY_RELATIVE_DIR, loadSharedMemories, writeSharedMemory, } from "./shared-markdown.js";
import { SessionContext } from "./session-context.js";
import { validateDeleteRecords, validateUpdateArgs } from "./validation.js";
export function createMemoryPlugin(options) {
    return async ({ client: opencode, directory, worktree }) => {
        const settings = resolveMemoryPluginOptions(options);
        const memoryProjectRoot = options.projectRoot ?? worktree;
        const memoryInstructions = await loadMemoryInstructions(options.root);
        const nativeLease = await acquireNativeMemoryClient(options.root, memoryProjectRoot);
        const native = nativeLease.client;
        const session = new SessionContext(native, (path, query) => opencode.session.get({ path, query }), directory);
        let sharedSignature;
        let sharedSync;
        let documentSync;
        let documentSyncTimer;
        const syncSharedMemories = async (force = false) => {
            if (!settings.sharedSync)
                return;
            if (sharedSync)
                return await sharedSync;
            sharedSync = (async () => {
                const loaded = await loadSharedMemories(memoryProjectRoot);
                for (const error of loaded.errors) {
                    session.warnOnce(new Error(`${error.source}: ${error.message}`));
                }
                if (!force && loaded.signature === sharedSignature)
                    return;
                const response = await native.request("sync_shared", {
                    records: loaded.records,
                });
                if (response.rejected > 0) {
                    throw new Error(`Rejected shared memories: ${response.rejections
                        .map((rejection) => `${rejection.source}: ${rejection.message}`)
                        .join(", ")}`);
                }
                sharedSignature = loaded.signature;
                session.invalidateRecall();
            })().finally(() => {
                sharedSync = undefined;
            });
            await sharedSync;
        };
        const indexDocuments = async (force = false) => {
            if (documentSync)
                return await documentSync;
            documentSync = native
                .request("index_documents", { force })
                .then((response) => {
                for (const rejection of response.rejections) {
                    session.warnOnce(new Error(`${rejection.path}: ${rejection.message}`));
                }
                for (const warning of response.warnings)
                    session.warnOnce(new Error(warning));
                if (response.added > 0 || response.updated > 0 || response.removed > 0) {
                    session.invalidateRecall();
                }
                return response;
            })
                .finally(() => {
                documentSync = undefined;
            });
            return await documentSync;
        };
        const syncDocuments = async () => {
            if (!settings.automaticDocumentIndex)
                return;
            await indexDocuments();
        };
        const syncProjectIndexes = async () => {
            await Promise.all([syncSharedMemories(), syncDocuments()]);
        };
        const scheduleDocumentSync = () => {
            if (!settings.automaticDocumentIndex)
                return;
            if (documentSyncTimer)
                clearTimeout(documentSyncTimer);
            documentSyncTimer = setTimeout(() => {
                documentSyncTimer = undefined;
                void syncDocuments().catch(session.warnOnce);
            }, settings.documentIndexDebounceMs);
        };
        if (settings.warmup || settings.automaticDocumentIndex) {
            const startup = settings.warmup
                ? [native.request("status"), syncProjectIndexes()]
                : [syncDocuments()];
            void Promise.all(startup).catch(session.warnOnce);
        }
        return {
            dispose: async () => {
                if (documentSyncTimer)
                    clearTimeout(documentSyncTimer);
                if (documentSync)
                    await documentSync.catch(() => undefined);
                for (const sessID of session.pendingRecall.keys())
                    session.discardPendingRecall(sessID);
                session.latestQuery.clear();
                session.invalidateRecall();
                session.pendingRecall.clear();
                session.sessionParents.clear();
                session.sessionRoots.clear();
                session.sessionAgents.clear();
                await nativeLease.release();
            },
            config: async (config) => {
                await registerMemoryInstructions(config, memoryInstructions, directory);
                config.command ??= {};
                config.command.memory ??= {
                    description: "Inspect and manage project memory",
                    template: `Manage memory for the current project. User request: $ARGUMENTS

When no arguments are supplied, call memory_status and memory_list, then summarize active scopes, stale/expired records, and suggested cleanup.
Use memory_search for semantic lookup, memory_get for full records, memory_update for corrections, memory_delete for removal, memory_promote for reviewed Git-shareable Markdown, and memory_doctor for diagnostics.
Never modify repository-scoped memory through memory_update; edit its .opencode/memory Markdown source instead. Ask through the tool permission flow before destructive or sharing operations.`,
                };
            },
            event: async ({ event }) => {
                if (event.type === "session.created" || event.type === "session.updated") {
                    session.sessionParents.set(event.properties.info.id, event.properties.info.parentID);
                    session.sessionRoots.clear();
                    return;
                }
                if (event.type === "session.deleted") {
                    const sessID = event.properties.info.id;
                    session.discardPendingRecall(sessID);
                    session.latestQuery.delete(sessID);
                    session.invalidateRecall(sessID);
                    session.sessionParents.delete(sessID);
                    session.sessionRoots.delete(sessID);
                    session.sessionAgents.delete(sessID);
                    return;
                }
                if (event.type === "session.idle") {
                    session.discardPendingRecall(event.properties.sessionID);
                    return;
                }
                if (event.type === "session.error" && event.properties.sessionID) {
                    await session.closePendingRecall(event.properties.sessionID, "error");
                    return;
                }
                if (event.type === "file.edited" || event.type === "file.watcher.updated") {
                    session.invalidateRecall();
                    const file = event.properties.file.replaceAll("\\", "/");
                    if (file.includes(`/${SHARED_MEMORY_RELATIVE_DIR}/`)) {
                        sharedSignature = undefined;
                    }
                    if (isSupportedDocumentPath(file))
                        scheduleDocumentSync();
                    return;
                }
                if (event.type !== "session.compacted")
                    return;
                if (!settings.automaticCapture)
                    return;
                try {
                    const response = await opencode.session.messages({
                        path: { id: event.properties.sessionID },
                        query: { directory, limit: 50 },
                    });
                    const summary = response.data
                        ?.toReversed()
                        .find((message) => message.info.role === "assistant" && message.info.summary === true);
                    if (!summary)
                        return;
                    const content = summary.parts
                        .flatMap((part) => (part.type === "text" && !part.ignored ? [part.text] : []))
                        .join("\n")
                        .trim();
                    if (!content)
                        return;
                    const candidates = parseCuratedCandidates(content);
                    let storedAny = false;
                    for (const candidate of candidates) {
                        try {
                            const response = await native.request("capture", {
                                candidate: {
                                    ...candidate,
                                    source: `session:${event.properties.sessionID}:compaction`,
                                    scope: "project",
                                    origin: "auto_compaction",
                                    revive: false,
                                },
                                significance: candidate.importance,
                                impact: candidate.kind === "decision" || candidate.kind === "gotcha" ? 0.8 : 0.6,
                                rarity: candidate.code_paths.length > 0 ? 0.7 : 0.5,
                                source_trust: "agent",
                                has_valid_evidence: candidate.code_paths.length > 0,
                                suggested_supersession_ids: [],
                                suggested_conflict_ids: [],
                            });
                            storedAny ||= response.stored !== undefined;
                        }
                        catch (error) {
                            session.warnOnce(error);
                        }
                    }
                    if (storedAny)
                        session.invalidateRecall();
                }
                catch (error) {
                    session.warnOnce(error);
                }
            },
            "chat.message": async (input, output) => {
                session.latestQuery.delete(input.sessionID);
                session.invalidateRecall(input.sessionID);
                session.discardPendingRecall(input.sessionID);
                const query = deriveRecallQuery(output.parts);
                if (!query)
                    return;
                if (input.agent)
                    session.sessionAgents.set(input.sessionID, input.agent);
                session.latestQuery.set(input.sessionID, {
                    query: truncateText(query, 2_000),
                    agent: input.agent,
                });
            },
            "experimental.chat.system.transform": async (input, output) => {
                if (!output.system.some((entry) => entry.includes(MEMORY_INSTRUCTIONS_MARKER))) {
                    output.system.push(memoryInstructions.content);
                }
                if (!input.sessionID)
                    return;
                if (!settings.automaticRecall)
                    return;
                const sessionID = input.sessionID;
                const latest = session.latestQuery.get(sessionID);
                if (!latest)
                    return;
                try {
                    await syncProjectIndexes();
                }
                catch (error) {
                    session.warnOnce(error);
                }
                if (session.latestQuery.get(input.sessionID) !== latest)
                    return;
                const rootSessionID = await session.resolveSessionRoot(input.sessionID);
                const agent = latest.agent ?? session.sessionAgents.get(input.sessionID) ?? "unknown";
                const budgetChars = contextBudgetChars(input.model);
                const recallGeneration = session.recallGeneration(input.sessionID);
                const cacheKey = [
                    latest.query,
                    rootSessionID,
                    agent,
                    budgetChars,
                    sharedSignature ?? "none",
                    recallGeneration,
                ].join("\0");
                let cached = session.recallCache.get(input.sessionID);
                if (!cached || cached.key !== cacheKey) {
                    session.discardPendingRecall(input.sessionID);
                    if (session.latestQuery.get(input.sessionID) !== latest ||
                        session.recallGeneration(input.sessionID) !== recallGeneration) {
                        return;
                    }
                    try {
                        const response = await session.searchRecallOnce(input.sessionID, cacheKey, async () => {
                            const response = await native.request("search", {
                                query: latest.query,
                                max_results: 20,
                                budget_chars: budgetChars,
                                kinds: [],
                                scopes: [],
                                taxonomies: [],
                                session_scope_key: rootSessionID,
                                agent_scope_key: agent,
                                min_score: settings.minScore,
                                include_stale: false,
                                include_superseded: false,
                                track_feedback: settings.feedbackTracking,
                            });
                            for (const warning of response.warnings)
                                session.warnOnce(new Error(warning));
                            return response;
                        });
                        if (session.latestQuery.get(input.sessionID) !== latest ||
                            session.recallGeneration(input.sessionID) !== recallGeneration) {
                            return;
                        }
                        cached = { key: cacheKey, response };
                        session.recallCache.set(input.sessionID, cached);
                    }
                    catch (error) {
                        session.warnOnce(error);
                        return;
                    }
                }
                const formatted = formatRecalledMemories(cached.response, budgetChars);
                if (!formatted)
                    return;
                if (!settings.feedbackTracking || !cached.response.retrieval_id) {
                    output.system.push(formatted.text);
                    return;
                }
                const pending = {
                    retrievalID: cached.response.retrieval_id,
                    memoryIDs: formatted.memoryIDs,
                };
                const opened = await session.openPendingRecall(sessionID, pending, () => {
                    return (session.latestQuery.get(sessionID) === latest &&
                        session.recallGeneration(sessionID) === recallGeneration);
                });
                if (opened)
                    output.system.push(formatted.text);
            },
            "experimental.session.compacting": async (_input, output) => {
                output.context.push(COMPACTION_CONTEXT);
            },
            tool: {
                memory_search: tool({
                    description: "Semantically search durable memory for the current project. Use before substantial work when prior decisions, preferences, facts, patterns, or gotchas may matter.",
                    args: {
                        query: tool.schema
                            .string()
                            .min(1)
                            .max(2_000)
                            .describe("Concise task-specific retrieval query."),
                        retrieval_mode: tool.schema
                            .enum(RETRIEVAL_MODES)
                            .default("hybrid")
                            .describe("Retrieval channel used for search and benchmark comparisons."),
                        limit: tool.schema
                            .number()
                            .int()
                            .min(1)
                            .max(20)
                            .default(20)
                            .describe("Safety ceiling; context budget normally decides the count."),
                        budget_chars: tool.schema
                            .number()
                            .int()
                            .min(512)
                            .max(24_000)
                            .default(6_000)
                            .describe("Maximum serialized memory context in characters."),
                        kinds: tool.schema
                            .array(tool.schema.enum(MEMORY_KINDS))
                            .max(MEMORY_KINDS.length)
                            .default([])
                            .describe("Optional memory kinds to include."),
                        scopes: tool.schema
                            .array(tool.schema.enum(MEMORY_SCOPES))
                            .max(MEMORY_SCOPES.length)
                            .default([])
                            .describe("Optional scopes to include."),
                        taxonomies: tool.schema
                            .array(tool.schema.enum(MEMORY_TAXONOMIES))
                            .max(MEMORY_TAXONOMIES.length)
                            .default([])
                            .describe("Optional CoALA-derived taxonomies to include."),
                        min_score: tool.schema
                            .number()
                            .min(0)
                            .max(1)
                            .default(settings.minScore)
                            .describe("Minimum calibrated relevance score."),
                        include_stale: tool.schema
                            .boolean()
                            .default(false)
                            .describe("Include memories whose code anchors changed."),
                        include_superseded: tool.schema
                            .boolean()
                            .default(false)
                            .describe("Include historical memories replaced by a successor."),
                    },
                    async execute(args, context) {
                        session.discardPendingRecall(context.sessionID);
                        await syncProjectIndexes();
                        const rootSessionID = await session.resolveSessionRoot(context.sessionID);
                        const response = await native.request("search", {
                            query: args.query,
                            retrieval_mode: args.retrieval_mode,
                            max_results: args.limit,
                            budget_chars: args.budget_chars,
                            kinds: args.kinds,
                            scopes: args.scopes,
                            taxonomies: args.taxonomies,
                            session_scope_key: rootSessionID,
                            agent_scope_key: context.agent,
                            min_score: args.min_score,
                            include_stale: args.include_stale,
                            include_superseded: args.include_superseded,
                            track_feedback: settings.feedbackTracking,
                        }, context.abort);
                        for (const warning of response.warnings)
                            session.warnOnce(new Error(warning));
                        if (settings.feedbackTracking &&
                            response.retrieval_id &&
                            response.memories.length > 0) {
                            const pending = {
                                retrievalID: response.retrieval_id,
                                memoryIDs: response.memories.map((memory) => memory.id),
                            };
                            await session.openPendingRecall(context.sessionID, pending);
                        }
                        return result("Memory search", response, {
                            count: response.count,
                            retrieval_id: response.retrieval_id,
                            abstained: response.abstained,
                        });
                    },
                }),
                memory_store: tool({
                    description: "Store one distilled, durable project memory. Never store secrets, raw conversations, temporary logs, unverified guesses, or guessed code_paths.",
                    args: {
                        content: tool.schema
                            .string()
                            .min(1)
                            .max(6_000)
                            .describe("Self-contained durable fact or concise summary."),
                        title: tool.schema
                            .string()
                            .min(1)
                            .max(160)
                            .optional()
                            .describe("Short descriptive title; inferred when omitted."),
                        kind: tool.schema
                            .enum(MEMORY_KINDS)
                            .default("fact")
                            .describe("Durable memory category."),
                        importance: tool.schema
                            .number()
                            .min(0)
                            .max(1)
                            .default(0.7)
                            .describe("Long-term importance from 0 to 1."),
                        tags: tool.schema
                            .array(tool.schema.string().min(1).max(64))
                            .max(12)
                            .default([])
                            .describe("Short retrieval tags."),
                        scope: tool.schema
                            .enum(WRITABLE_MEMORY_SCOPES)
                            .default("project")
                            .describe("session shares with the parent/subagent family; agent is role-specific; project is durable and private."),
                        expires_in_days: tool.schema
                            .number()
                            .int()
                            .min(1)
                            .max(3_650)
                            .optional()
                            .describe("Optional hard expiry override."),
                        code_paths: tool.schema
                            .array(tool.schema.string().min(1).max(512))
                            .max(12)
                            .default([])
                            .describe("Existing regular files relative to the project root that verify this memory. Never guess paths; leave empty when no verified file applies. Any invalid path rejects the store."),
                        revive: tool.schema
                            .boolean()
                            .default(false)
                            .describe("Revive a tombstoned memory after user approval."),
                        taxonomy: tool.schema
                            .enum(MEMORY_TAXONOMIES)
                            .optional()
                            .describe("Explicit memory taxonomy; usually omit so it is inferred."),
                        confidence: tool.schema
                            .number()
                            .min(0)
                            .max(1)
                            .optional()
                            .describe("Confidence in this memory; defaults from importance."),
                    },
                    async execute(args, context) {
                        if (args.revive) {
                            await context.ask({
                                permission: "memory_revive",
                                patterns: [args.title ?? truncateText(args.content, 80)],
                                always: [],
                                metadata: { operation: "revive", scope: args.scope },
                            });
                        }
                        const key = await session.scopeKey(args.scope, context.sessionID, context.agent);
                        const response = await native.request("store", {
                            ...args,
                            scope_key: key,
                            origin: "manual",
                            source: `session:${context.sessionID}`,
                        }, context.abort);
                        session.invalidateRecall();
                        return result("Stored memory", response, {
                            id: response.id,
                            inserted: response.inserted,
                        });
                    },
                }),
                memory_ingest: tool({
                    description: "Extract a project-local PDF, Markdown, or HTML document with xberg and persist its text as bounded, searchable memory chunks.",
                    args: {
                        path: tool.schema
                            .string()
                            .min(1)
                            .max(200)
                            .describe("Existing document path relative to the project root."),
                        title: tool.schema
                            .string()
                            .min(1)
                            .max(120)
                            .optional()
                            .describe("Optional document title; defaults to the filename."),
                        kind: tool.schema.enum(MEMORY_KINDS).default("fact"),
                        importance: tool.schema.number().min(0).max(1).default(0.6),
                        tags: tool.schema.array(tool.schema.string().min(1).max(64)).max(11).default([]),
                        scope: tool.schema
                            .enum(WRITABLE_MEMORY_SCOPES)
                            .default("project")
                            .describe("session shares within the parent/subagent family; agent is role-specific; project is durable and private."),
                        expires_in_days: tool.schema.number().int().min(1).max(3_650).optional(),
                        revive: tool.schema
                            .boolean()
                            .default(false)
                            .describe("Revive matching tombstoned chunks after user approval."),
                        taxonomy: tool.schema.enum(MEMORY_TAXONOMIES).optional(),
                        confidence: tool.schema.number().min(0).max(1).optional(),
                    },
                    async execute(args, context) {
                        await context.ask({
                            permission: "memory_ingest",
                            patterns: [args.path],
                            always: [],
                            metadata: { operation: "ingest", scope: args.scope },
                        });
                        if (args.revive) {
                            await context.ask({
                                permission: "memory_revive",
                                patterns: [args.path],
                                always: [],
                                metadata: { operation: "revive", scope: args.scope },
                            });
                        }
                        const scopeKey = await session.scopeKey(args.scope, context.sessionID, context.agent);
                        const response = await native.request("ingest", { ...args, scope_key: scopeKey }, context.abort);
                        for (const warning of response.warnings)
                            session.warnOnce(new Error(warning));
                        session.invalidateRecall();
                        return result("Ingested document", response, {
                            path: response.path,
                            chunk_count: response.chunk_count,
                            inserted: response.inserted,
                        });
                    },
                }),
                memory_index_documents: tool({
                    description: "Incrementally discover and index supported project documents while respecting .gitignore and .ignore files. Unchanged files are skipped and deleted files are removed from the derived index.",
                    args: {
                        force: tool.schema
                            .boolean()
                            .default(false)
                            .describe("Re-extract every discovered document even when its content hash is unchanged."),
                    },
                    async execute(args, context) {
                        await context.ask({
                            permission: "memory_ingest",
                            patterns: ["**/*.{pdf,md,markdown,html,htm}"],
                            always: [],
                            metadata: { operation: "index_documents", force: args.force },
                        });
                        const response = await indexDocuments(args.force);
                        session.invalidateRecall();
                        return result("Indexed project documents", response, {
                            discovered: response.discovered,
                            added: response.added,
                            updated: response.updated,
                            removed: response.removed,
                            rejected: response.rejected,
                        });
                    },
                }),
                memory_get: tool({
                    description: "Fetch complete durable memories by IDs returned from memory_search.",
                    args: {
                        ids: tool.schema
                            .array(tool.schema.string().regex(/^mem_[0-9a-f]{32}$/))
                            .min(1)
                            .max(100)
                            .describe("Memory IDs to fetch."),
                    },
                    async execute(args, context) {
                        const keys = await session.managementScopeKeys(context.sessionID, context.agent);
                        const response = await native.request("get", { ...args, ...keys }, context.abort);
                        return result("Memories", response, {
                            count: response.length,
                        });
                    },
                }),
                memory_list: tool({
                    description: "List lifecycle-indexed memories for review, cleanup, and /memory management.",
                    args: {
                        kinds: tool.schema
                            .array(tool.schema.enum(MEMORY_KINDS))
                            .max(MEMORY_KINDS.length)
                            .default([]),
                        scopes: tool.schema
                            .array(tool.schema.enum(MEMORY_SCOPES))
                            .max(MEMORY_SCOPES.length)
                            .default([]),
                        taxonomies: tool.schema
                            .array(tool.schema.enum(MEMORY_TAXONOMIES))
                            .max(MEMORY_TAXONOMIES.length)
                            .default([]),
                        include_expired: tool.schema.boolean().default(false),
                        include_stale: tool.schema.boolean().default(false),
                        include_superseded: tool.schema.boolean().default(false),
                        offset: tool.schema.number().int().min(0).default(0),
                        limit: tool.schema.number().int().min(1).max(100).default(50),
                    },
                    async execute(args, context) {
                        await syncProjectIndexes();
                        const keys = await session.managementScopeKeys(context.sessionID, context.agent);
                        const response = await native.request("list", { ...args, ...keys }, context.abort);
                        return result("Memory list", response, {
                            total: response.total,
                            count: response.count,
                        });
                    },
                }),
                memory_update: tool({
                    description: "Correct or reclassify one local memory by ID with optional optimistic concurrency.",
                    args: {
                        id: tool.schema.string().regex(/^mem_[0-9a-f]{32}$/),
                        expected_updated_at_ms: tool.schema.number().int().optional(),
                        content: tool.schema.string().min(1).max(6_000).optional(),
                        title: tool.schema.string().min(1).max(160).optional(),
                        kind: tool.schema.enum(MEMORY_KINDS).optional(),
                        importance: tool.schema.number().min(0).max(1).optional(),
                        tags: tool.schema.array(tool.schema.string().min(1).max(64)).max(12).optional(),
                        scope: tool.schema.enum(WRITABLE_MEMORY_SCOPES).optional(),
                        expires_in_days: tool.schema.number().int().min(1).max(3_650).optional(),
                        clear_expiry: tool.schema.boolean().default(false),
                        code_paths: tool.schema
                            .array(tool.schema.string().min(1).max(512))
                            .max(12)
                            .optional()
                            .describe("Replacement anchors: verified existing regular files relative to the project root. Omit to preserve current anchors; pass [] to clear them. Any invalid path rejects the update."),
                        pinned: tool.schema
                            .boolean()
                            .optional()
                            .describe("Pin the memory so it bypasses expiry and retention decay."),
                        lock_action: tool.schema
                            .enum(LOCK_ACTIONS)
                            .optional()
                            .describe("Lock or unlock the memory. Locked records block updates and deletes."),
                        lock_reason: tool.schema
                            .string()
                            .min(1)
                            .max(240)
                            .optional()
                            .describe("Reason for locking the memory. Only valid with lock_action='lock'."),
                        taxonomy: tool.schema
                            .enum(MEMORY_TAXONOMIES)
                            .optional()
                            .describe("Explicit reclassification; omit to preserve the current taxonomy."),
                        confidence: tool.schema.number().min(0).max(1).optional(),
                        conflict_with: tool.schema
                            .array(tool.schema.string().regex(/^mem_[0-9a-f]{32}$/))
                            .max(100)
                            .optional()
                            .describe("Symmetric conflict links; pass [] to clear links."),
                    },
                    async execute(args, context) {
                        validateUpdateArgs(args);
                        const keys = await session.managementScopeKeys(context.sessionID, context.agent);
                        const existing = await native.request("get", { ids: [args.id], ...keys }, context.abort);
                        const record = existing[0];
                        if (!record)
                            throw new Error(`Memory not found: ${args.id}`);
                        if (record.scope === "repository") {
                            throw new Error("Repository memory is canonical Markdown; edit its .opencode/memory file instead.");
                        }
                        const key = args.scope
                            ? await session.scopeKey(args.scope, context.sessionID, context.agent)
                            : undefined;
                        const response = await native.request("update", { ...args, scope_key: key, ...keys }, context.abort);
                        session.invalidateRecall();
                        return result("Updated memory", response, response);
                    },
                }),
                memory_pin: tool({
                    description: "Pin or unpin one local memory without re-embedding or refreshing semantic recency.",
                    args: {
                        id: tool.schema.string().regex(/^mem_[0-9a-f]{32}$/),
                        pinned: tool.schema.boolean(),
                        expected_updated_at_ms: tool.schema.number().int().optional(),
                    },
                    async execute(args, context) {
                        const keys = await session.managementScopeKeys(context.sessionID, context.agent);
                        const response = await native.request("pin", { ...args, ...keys }, context.abort);
                        session.invalidateRecall();
                        return result(args.pinned ? "Pinned memory" : "Unpinned memory", response, response);
                    },
                }),
                memory_lock: tool({
                    description: "Lock or unlock one local memory. Unlock is lifecycle-only; locked records reject semantic changes and deletion.",
                    args: {
                        id: tool.schema.string().regex(/^mem_[0-9a-f]{32}$/),
                        lock_action: tool.schema.enum(LOCK_ACTIONS),
                        lock_reason: tool.schema.string().min(1).max(240).optional(),
                        expected_updated_at_ms: tool.schema.number().int().optional(),
                    },
                    async execute(args, context) {
                        if (args.lock_action === "unlock" && args.lock_reason !== undefined) {
                            throw new Error("lock_reason is valid only when locking");
                        }
                        const keys = await session.managementScopeKeys(context.sessionID, context.agent);
                        const response = await native.request("lock", { ...args, ...keys }, context.abort);
                        session.invalidateRecall();
                        return result(args.lock_action === "lock" ? "Locked memory" : "Unlocked memory", response, response);
                    },
                }),
                memory_delete: tool({
                    description: "Batch-delete obsolete or incorrect memories and leave tombstones by default.",
                    args: {
                        ids: tool.schema
                            .array(tool.schema.string().regex(/^mem_[0-9a-f]{32}$/))
                            .min(1)
                            .max(100),
                        tombstone: tool.schema.boolean().default(true),
                        reason: tool.schema
                            .enum(["obsolete", "incorrect", "user_deleted"])
                            .default("user_deleted"),
                    },
                    async execute(args, context) {
                        const keys = await session.managementScopeKeys(context.sessionID, context.agent);
                        const records = await native.request("get", { ids: args.ids, ...keys }, context.abort);
                        validateDeleteRecords(records);
                        await context.ask({
                            permission: "memory_delete",
                            patterns: args.ids,
                            always: [],
                            metadata: { operation: "delete", ...args },
                        });
                        const response = await native.request("delete", { ...args, ...keys }, context.abort);
                        session.invalidateRecall();
                        return result("Deleted memories", response, response);
                    },
                }),
                memory_feedback: tool({
                    description: "Record whether recalled memory was used, ignored, or caused an error. Used feedback must be explicit.",
                    args: {
                        retrieval_id: tool.schema
                            .string()
                            .regex(/^ret_[0-9a-f]{24}$/)
                            .optional()
                            .describe("Defaults to the latest pending retrieval in this session."),
                        event: tool.schema.enum(FEEDBACK_EVENTS),
                        memory_ids: tool.schema
                            .array(tool.schema.string().regex(/^mem_[0-9a-f]{32}$/))
                            .min(1)
                            .max(100)
                            .describe("Exact recalled memory IDs affected by this feedback."),
                    },
                    async execute(args, context) {
                        const pending = session.pendingRecall.get(context.sessionID);
                        const retrievalID = args.retrieval_id ?? pending?.retrievalID;
                        if (!retrievalID) {
                            throw new Error("No pending retrieval is available for this session");
                        }
                        const response = await native.request("feedback", {
                            retrieval_id: retrievalID,
                            event: args.event,
                            memory_ids: args.memory_ids,
                        }, context.abort);
                        if (pending?.retrievalID === retrievalID) {
                            session.pendingRecall.delete(context.sessionID);
                        }
                        return result("Recorded memory feedback", response, response);
                    },
                }),
                memory_promote: tool({
                    description: "Promote one reviewed local memory to Git-shareable .opencode/memory Markdown.",
                    args: {
                        id: tool.schema.string().regex(/^mem_[0-9a-f]{32}$/),
                    },
                    async execute(args, context) {
                        const keys = await session.managementScopeKeys(context.sessionID, context.agent);
                        const memories = await native.request("get", { ids: [args.id], ...keys }, context.abort);
                        const memory = memories[0];
                        if (!memory)
                            throw new Error(`Memory not found: ${args.id}`);
                        if (memory.scope === "repository") {
                            return result("Memory already shared", { id: memory.id, source: memory.source }, { id: memory.id });
                        }
                        const destination = `${SHARED_MEMORY_RELATIVE_DIR}/${memory.id}.md`;
                        await context.ask({
                            permission: "memory_promote",
                            patterns: [destination],
                            always: [],
                            metadata: {
                                operation: "promote",
                                id: memory.id,
                                title: memory.title,
                                destination,
                            },
                        });
                        const path = await writeSharedMemory(memoryProjectRoot, memory);
                        await syncSharedMemories(true);
                        return result("Promoted memory", { id: memory.id, path }, { id: memory.id, path });
                    },
                }),
                memory_export: tool({
                    description: "Export visible memories, lifecycle relations, and tombstones as a portable JSON snapshot.",
                    args: {
                        include_expired: tool.schema.boolean().default(true),
                        include_superseded: tool.schema.boolean().default(true),
                    },
                    async execute(args, context) {
                        const keys = await session.managementScopeKeys(context.sessionID, context.agent);
                        const snapshot = await native.request("export", { ...args, ...keys }, context.abort);
                        return result("Memory snapshot", snapshot, {
                            format_version: snapshot.format_version,
                            source_project_id: snapshot.source_project_id,
                        });
                    },
                }),
                memory_import: tool({
                    description: "Import a native-memory JSON snapshot after validating IDs, relations, lifecycle metadata, and content safety.",
                    args: {
                        snapshot_json: tool.schema
                            .string()
                            .min(2)
                            .max(4_000_000)
                            .describe("Exact JSON returned by memory_export."),
                    },
                    async execute(args, context) {
                        let snapshot;
                        try {
                            snapshot = JSON.parse(args.snapshot_json);
                        }
                        catch (error) {
                            throw new Error("snapshot_json is not valid JSON", { cause: error });
                        }
                        if (typeof snapshot !== "object" || snapshot === null || Array.isArray(snapshot)) {
                            throw new Error("snapshot_json must contain a snapshot object");
                        }
                        await context.ask({
                            permission: "memory_import",
                            patterns: ["native-memory-snapshot"],
                            always: [],
                            metadata: { operation: "import" },
                        });
                        const response = await native.request("import", { snapshot }, context.abort);
                        session.invalidateRecall();
                        return result("Imported memory snapshot", response, response);
                    },
                }),
                memory_purge: tool({
                    description: "Delete all local indexed memories for the current project. Shared Markdown files are preserved.",
                    args: {
                        project_id: tool.schema
                            .string()
                            .regex(/^[0-9a-f]{64}$/)
                            .describe("Exact project ID from memory_status."),
                        keep_tombstones: tool.schema.boolean().default(true),
                    },
                    async execute(args, context) {
                        await context.ask({
                            permission: "memory_purge",
                            patterns: [args.project_id],
                            always: [],
                            metadata: { operation: "purge", ...args },
                        });
                        const response = await native.request("purge", args, context.abort);
                        session.invalidateRecall();
                        session.pendingRecall.clear();
                        sharedSignature = undefined;
                        return result("Purged memory", response, response);
                    },
                }),
                memory_optimize: tool({
                    description: "Prune expired memories and retrieval logs, compact zvec, and rebuild indexes.",
                    args: {},
                    async execute(_args, context) {
                        const response = await native.request("optimize", {}, context.abort);
                        session.invalidateRecall();
                        return result("Optimized memory", response, response);
                    },
                }),
                memory_doctor: tool({
                    description: "Diagnose state compatibility, index health, retention, code anchors, and model cache.",
                    args: {
                        deep: tool.schema
                            .boolean()
                            .default(false)
                            .describe("Hash all code anchors to detect staleness."),
                    },
                    async execute(args, context) {
                        await syncProjectIndexes();
                        const response = await native.request("doctor", args, context.abort);
                        return result("Memory doctor", response, response);
                    },
                }),
                memory_status: tool({
                    description: "Inspect the current project's native memory backend, collection, embedding model, indexes, and document count.",
                    args: {},
                    async execute(_args, context) {
                        await syncProjectIndexes();
                        const response = await native.request("status", {}, context.abort);
                        return result("Memory status", response, response);
                    },
                }),
            },
        };
    };
}
export function resolveMemoryPluginOptions(options) {
    const minScore = options.minScore ?? envNumber("OPENCODE_MEMORY_MIN_SCORE", 0.42);
    if (!Number.isFinite(minScore) || minScore < 0 || minScore > 1) {
        throw new Error("memory minScore must be between 0 and 1");
    }
    const documentIndexDebounceMs = options.documentIndexDebounceMs ?? envNumber("OPENCODE_MEMORY_DOCUMENT_INDEX_DEBOUNCE_MS", 750);
    if (!Number.isFinite(documentIndexDebounceMs) ||
        documentIndexDebounceMs < 50 ||
        documentIndexDebounceMs > 60_000) {
        throw new Error("memory documentIndexDebounceMs must be between 50 and 60000");
    }
    return {
        warmup: options.warmup ?? envBoolean("OPENCODE_MEMORY_WARMUP", true),
        automaticRecall: options.automaticRecall ?? envBoolean("OPENCODE_MEMORY_AUTO_RECALL", true),
        automaticCapture: options.automaticCapture ?? envBoolean("OPENCODE_MEMORY_AUTO_CAPTURE", true),
        automaticDocumentIndex: options.automaticDocumentIndex ?? envBoolean("OPENCODE_MEMORY_AUTO_INDEX_DOCUMENTS", true),
        documentIndexDebounceMs,
        sharedSync: options.sharedSync ?? envBoolean("OPENCODE_MEMORY_SHARED_SYNC", true),
        feedbackTracking: options.feedbackTracking ?? envBoolean("OPENCODE_MEMORY_FEEDBACK_TRACKING", true),
        minScore,
    };
}
export function isSupportedDocumentPath(path) {
    return /\.(?:pdf|md|markdown|html|htm)$/i.test(path);
}
function envBoolean(name, fallback) {
    const value = process.env[name];
    if (value === undefined || value === "")
        return fallback;
    if (["1", "true", "yes", "on"].includes(value.toLowerCase()))
        return true;
    if (["0", "false", "no", "off"].includes(value.toLowerCase()))
        return false;
    throw new Error(`${name} must be a boolean`);
}
function envNumber(name, fallback) {
    const value = process.env[name];
    if (value === undefined || value === "")
        return fallback;
    const parsed = Number(value);
    if (!Number.isFinite(parsed))
        throw new Error(`${name} must be a finite number`);
    return parsed;
}
function result(title, value, metadata) {
    return {
        title,
        output: JSON.stringify(value, null, 2),
        metadata,
    };
}
//# sourceMappingURL=plugin.js.map