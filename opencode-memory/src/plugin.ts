import type { Plugin, ToolResult } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import type {
  MemoryRecord,
  ListResponse,
  SearchResponse,
  SharedSyncResponse,
  CaptureResponse,
} from "./contracts.js";
import {
  MEMORY_KINDS,
  MEMORY_SCOPES,
  WRITABLE_MEMORY_SCOPES,
  FEEDBACK_EVENTS,
  LOCK_ACTIONS,
  MEMORY_TAXONOMIES,
} from "./contracts.js";
import { NativeMemoryClient } from "./sidecar-client.js";
import {
  MEMORY_POLICY,
  MEMORY_POLICY_MARKER,
  COMPACTION_CONTEXT,
  formatRecalledMemories,
  truncateText,
  contextBudgetChars,
  parseCuratedCandidates,
  deriveRecallQuery,
} from "./policy.js";
import {
  SHARED_MEMORY_RELATIVE_DIR,
  loadSharedMemories,
  writeSharedMemory,
} from "./shared-markdown.js";
import { SessionContext } from "./session-context.js";
import { validateDeleteRecords, validateUpdateArgs } from "./validation.js";

export interface MemoryPluginOptions {
  root: string;
  warmup?: boolean;
  automaticRecall?: boolean;
  automaticCapture?: boolean;
  sharedSync?: boolean;
  feedbackTracking?: boolean;
  minScore?: number;
  projectRoot?: string;
}

export function createMemoryPlugin(options: MemoryPluginOptions): Plugin {
  return async ({ client: opencode, directory, worktree }) => {
    const settings = resolveMemoryPluginOptions(options);
    const memoryProjectRoot = options.projectRoot ?? worktree;
    const native = new NativeMemoryClient(options.root, memoryProjectRoot);
    const session = new SessionContext(
      native,
      (path, query) => opencode.session.get({ path, query }),
      directory,
    );
    let sharedSignature: string | undefined;
    let sharedSync: Promise<void> | undefined;

    const syncSharedMemories = async (force = false): Promise<void> => {
      if (!settings.sharedSync) return;
      if (sharedSync) return await sharedSync;
      sharedSync = (async () => {
        const loaded = await loadSharedMemories(memoryProjectRoot);
        for (const error of loaded.errors) {
          session.warnOnce(new Error(`${error.source}: ${error.message}`));
        }
        if (!force && loaded.signature === sharedSignature) return;
        const response = await native.request<SharedSyncResponse>("sync_shared", {
          records: loaded.records,
        });
        if (response.rejected > 0) {
          throw new Error(
            `Rejected shared memories: ${response.rejections
              .map((rejection) => `${rejection.source}: ${rejection.message}`)
              .join(", ")}`,
          );
        }
        sharedSignature = loaded.signature;
        session.invalidateRecall();
      })().finally(() => {
        sharedSync = undefined;
      });
      await sharedSync;
    };

    if (settings.warmup) {
      void Promise.all([native.request("status"), syncSharedMemories()]).catch(session.warnOnce);
    }

    return {
      dispose: async () => {
        await Promise.all(
          [...session.pendingRecall.keys()].map((sessID) =>
            session.closePendingRecall(sessID, "ignored"),
          ),
        );
        session.latestQuery.clear();
        session.invalidateRecall();
        session.pendingRecall.clear();
        session.sessionParents.clear();
        session.sessionRoots.clear();
        session.sessionAgents.clear();
        await native.dispose();
      },
      config: async (config) => {
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
          await session.closePendingRecall(sessID, "ignored");
          session.latestQuery.delete(sessID);
          session.invalidateRecall(sessID);
          session.sessionParents.delete(sessID);
          session.sessionRoots.delete(sessID);
          session.sessionAgents.delete(sessID);
          return;
        }
        if (event.type === "session.idle") {
          await session.closePendingRecall(event.properties.sessionID, "ignored");
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
          return;
        }
        if (event.type !== "session.compacted") return;
        if (!settings.automaticCapture) return;

        try {
          const response = await opencode.session.messages({
            path: { id: event.properties.sessionID },
            query: { directory, limit: 50 },
          });
          const summary = response.data
            ?.toReversed()
            .find((message) => message.info.role === "assistant" && message.info.summary === true);
          if (!summary) return;
          const content = summary.parts
            .flatMap((part) => (part.type === "text" && !part.ignored ? [part.text] : []))
            .join("\n")
            .trim();
          if (!content) return;
          const candidates = parseCuratedCandidates(content);
          let storedAny = false;
          for (const candidate of candidates) {
            try {
              const response = await native.request<CaptureResponse>("capture", {
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
            } catch (error) {
              session.warnOnce(error);
            }
          }
          if (storedAny) session.invalidateRecall();
        } catch (error) {
          session.warnOnce(error);
        }
      },
      "chat.message": async (input, output) => {
        session.latestQuery.delete(input.sessionID);
        session.invalidateRecall(input.sessionID);
        await session.closePendingRecall(input.sessionID, "ignored");
        const query = deriveRecallQuery(output.parts);
        if (!query) return;
        if (input.agent) session.sessionAgents.set(input.sessionID, input.agent);
        session.latestQuery.set(input.sessionID, {
          query: truncateText(query, 2_000),
          agent: input.agent,
        });
      },
      "experimental.chat.system.transform": async (input, output) => {
        if (!output.system.some((entry) => entry.includes(MEMORY_POLICY_MARKER))) {
          output.system.push(MEMORY_POLICY);
        }
        if (!input.sessionID) return;
        if (!settings.automaticRecall) return;
        const sessionID = input.sessionID;
        const latest = session.latestQuery.get(sessionID);
        if (!latest) return;
        try {
          await syncSharedMemories();
        } catch (error) {
          session.warnOnce(error);
        }
        if (session.latestQuery.get(input.sessionID) !== latest) return;
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
          await session.closePendingRecall(input.sessionID, "ignored");
          if (
            session.latestQuery.get(input.sessionID) !== latest ||
            session.recallGeneration(input.sessionID) !== recallGeneration
          ) {
            return;
          }
          try {
            const response = await session.searchRecallOnce(input.sessionID, cacheKey, async () => {
              const response = await native.request<SearchResponse>("search", {
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
              for (const warning of response.warnings) session.warnOnce(new Error(warning));
              return response;
            });
            if (
              session.latestQuery.get(input.sessionID) !== latest ||
              session.recallGeneration(input.sessionID) !== recallGeneration
            ) {
              return;
            }
            cached = { key: cacheKey, response };
            session.recallCache.set(input.sessionID, cached);
          } catch (error) {
            session.warnOnce(error);
            return;
          }
        }
        const formatted = formatRecalledMemories(cached.response, budgetChars);
        if (!formatted) return;
        if (!settings.feedbackTracking || !cached.response.retrieval_id) {
          output.system.push(formatted.text);
          return;
        }
        const pending = {
          retrievalID: cached.response.retrieval_id,
          memoryIDs: formatted.memoryIDs,
        };
        const opened = await session.openPendingRecall(sessionID, pending, () => {
          return (
            session.latestQuery.get(sessionID) === latest &&
            session.recallGeneration(sessionID) === recallGeneration
          );
        });
        if (opened) output.system.push(formatted.text);
      },
      "experimental.session.compacting": async (_input, output) => {
        output.context.push(COMPACTION_CONTEXT);
      },
      tool: {
        memory_search: tool({
          description:
            "Semantically search durable memory for the current project. Use before substantial work when prior decisions, preferences, facts, patterns, or gotchas may matter.",
          args: {
            query: tool.schema
              .string()
              .min(1)
              .max(2_000)
              .describe("Concise task-specific retrieval query."),
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
            await session.closePendingRecall(context.sessionID, "ignored");
            await syncSharedMemories();
            const rootSessionID = await session.resolveSessionRoot(context.sessionID);
            const response = await native.request<SearchResponse>(
              "search",
              {
                query: args.query,
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
              },
              context.abort,
            );
            for (const warning of response.warnings) session.warnOnce(new Error(warning));
            if (
              settings.feedbackTracking &&
              response.retrieval_id &&
              response.memories.length > 0
            ) {
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
          description:
            "Store one distilled, durable project memory. Never store secrets, raw conversations, temporary logs, or unverified guesses.",
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
              .describe(
                "session shares with the parent/subagent family; agent is role-specific; project is durable and private.",
              ),
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
              .describe("Relative files that validate this memory."),
            revive: tool.schema
              .boolean()
              .default(false)
              .describe("Revive a tombstoned memory after user approval."),
            taxonomy: tool.schema
              .enum(MEMORY_TAXONOMIES)
              .optional()
              .describe("Explicit memory taxonomy; inferred when omitted."),
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
            const response = await native.request<Record<string, unknown>>(
              "store",
              {
                ...args,
                scope_key: key,
                origin: "manual",
                source: `session:${context.sessionID}`,
              },
              context.abort,
            );
            session.invalidateRecall();
            return result("Stored memory", response, {
              id: response.id,
              inserted: response.inserted,
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
            const response = await native.request<MemoryRecord[]>(
              "get",
              { ...args, ...keys },
              context.abort,
            );
            return result("Memories", response, {
              count: response.length,
            });
          },
        }),
        memory_list: tool({
          description:
            "List lifecycle-indexed memories for review, cleanup, and /memory management.",
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
            await syncSharedMemories();
            const keys = await session.managementScopeKeys(context.sessionID, context.agent);
            const response = await native.request<ListResponse>(
              "list",
              { ...args, ...keys },
              context.abort,
            );
            return result("Memory list", response, {
              total: response.total,
              count: response.count,
            });
          },
        }),
        memory_update: tool({
          description:
            "Correct or reclassify one local memory by ID with optional optimistic concurrency.",
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
            code_paths: tool.schema.array(tool.schema.string().min(1).max(512)).max(12).optional(),
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
            taxonomy: tool.schema.enum(MEMORY_TAXONOMIES).optional(),
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
            const existing = await native.request<MemoryRecord[]>(
              "get",
              { ids: [args.id], ...keys },
              context.abort,
            );
            const record = existing[0];
            if (!record) throw new Error(`Memory not found: ${args.id}`);
            if (record.scope === "repository") {
              throw new Error(
                "Repository memory is canonical Markdown; edit its .opencode/memory file instead.",
              );
            }
            const key = args.scope
              ? await session.scopeKey(args.scope, context.sessionID, context.agent)
              : undefined;
            const response = await native.request<Record<string, unknown>>(
              "update",
              { ...args, scope_key: key, ...keys },
              context.abort,
            );
            session.invalidateRecall();
            return result("Updated memory", response, response);
          },
        }),
        memory_pin: tool({
          description:
            "Pin or unpin one local memory without re-embedding or refreshing semantic recency.",
          args: {
            id: tool.schema.string().regex(/^mem_[0-9a-f]{32}$/),
            pinned: tool.schema.boolean(),
            expected_updated_at_ms: tool.schema.number().int().optional(),
          },
          async execute(args, context) {
            const keys = await session.managementScopeKeys(context.sessionID, context.agent);
            const response = await native.request<Record<string, unknown>>(
              "pin",
              { ...args, ...keys },
              context.abort,
            );
            session.invalidateRecall();
            return result(args.pinned ? "Pinned memory" : "Unpinned memory", response, response);
          },
        }),
        memory_lock: tool({
          description:
            "Lock or unlock one local memory. Unlock is lifecycle-only; locked records reject semantic changes and deletion.",
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
            const response = await native.request<Record<string, unknown>>(
              "lock",
              { ...args, ...keys },
              context.abort,
            );
            session.invalidateRecall();
            return result(
              args.lock_action === "lock" ? "Locked memory" : "Unlocked memory",
              response,
              response,
            );
          },
        }),
        memory_delete: tool({
          description:
            "Batch-delete obsolete or incorrect memories and leave tombstones by default.",
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
            const records = await native.request<MemoryRecord[]>(
              "get",
              { ids: args.ids, ...keys },
              context.abort,
            );
            validateDeleteRecords(records);
            await context.ask({
              permission: "memory_delete",
              patterns: args.ids,
              always: [],
              metadata: { operation: "delete", ...args },
            });
            const response = await native.request<Record<string, unknown>>(
              "delete",
              { ...args, ...keys },
              context.abort,
            );
            session.invalidateRecall();
            return result("Deleted memories", response, response);
          },
        }),
        memory_feedback: tool({
          description:
            "Record whether recalled memory was used, ignored, or caused an error. Used feedback must be explicit.",
          args: {
            retrieval_id: tool.schema
              .string()
              .regex(/^ret_[0-9a-f]{24}$/)
              .optional()
              .describe("Defaults to the latest pending retrieval in this session."),
            event: tool.schema.enum(FEEDBACK_EVENTS),
            memory_ids: tool.schema
              .array(tool.schema.string().regex(/^mem_[0-9a-f]{32}$/))
              .max(100)
              .default([]),
          },
          async execute(args, context) {
            const pending = session.pendingRecall.get(context.sessionID);
            const retrievalID = args.retrieval_id ?? pending?.retrievalID;
            if (!retrievalID) {
              throw new Error("No pending retrieval is available for this session");
            }
            const response = await native.request<Record<string, unknown>>(
              "feedback",
              {
                retrieval_id: retrievalID,
                event: args.event,
                memory_ids: args.memory_ids,
              },
              context.abort,
            );
            if (pending?.retrievalID === retrievalID) {
              session.pendingRecall.delete(context.sessionID);
            }
            return result("Recorded memory feedback", response, response);
          },
        }),
        memory_promote: tool({
          description:
            "Promote one reviewed local memory to Git-shareable .opencode/memory Markdown.",
          args: {
            id: tool.schema.string().regex(/^mem_[0-9a-f]{32}$/),
          },
          async execute(args, context) {
            const keys = await session.managementScopeKeys(context.sessionID, context.agent);
            const memories = await native.request<MemoryRecord[]>(
              "get",
              { ids: [args.id], ...keys },
              context.abort,
            );
            const memory = memories[0];
            if (!memory) throw new Error(`Memory not found: ${args.id}`);
            if (memory.scope === "repository") {
              return result(
                "Memory already shared",
                { id: memory.id, source: memory.source },
                { id: memory.id },
              );
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
          description:
            "Export visible memories, lifecycle relations, and tombstones as a portable JSON snapshot.",
          args: {
            include_expired: tool.schema.boolean().default(true),
            include_superseded: tool.schema.boolean().default(true),
          },
          async execute(args, context) {
            const keys = await session.managementScopeKeys(context.sessionID, context.agent);
            const snapshot = await native.request<Record<string, unknown>>(
              "export",
              { ...args, ...keys },
              context.abort,
            );
            return result("Memory snapshot", snapshot, {
              format_version: snapshot.format_version,
              source_project_id: snapshot.source_project_id,
            });
          },
        }),
        memory_import: tool({
          description:
            "Import a native-memory JSON snapshot after validating IDs, relations, lifecycle metadata, and content safety.",
          args: {
            snapshot_json: tool.schema
              .string()
              .min(2)
              .max(4_000_000)
              .describe("Exact JSON returned by memory_export."),
          },
          async execute(args, context) {
            let snapshot: unknown;
            try {
              snapshot = JSON.parse(args.snapshot_json);
            } catch (error) {
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
            const response = await native.request<Record<string, unknown>>(
              "import",
              { snapshot },
              context.abort,
            );
            session.invalidateRecall();
            return result("Imported memory snapshot", response, response);
          },
        }),
        memory_purge: tool({
          description:
            "Delete all local indexed memories for the current project. Shared Markdown files are preserved.",
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
            const response = await native.request<Record<string, unknown>>(
              "purge",
              args,
              context.abort,
            );
            session.invalidateRecall();
            session.pendingRecall.clear();
            sharedSignature = undefined;
            return result("Purged memory", response, response);
          },
        }),
        memory_optimize: tool({
          description:
            "Prune expired memories and retrieval logs, compact zvec, and rebuild indexes.",
          args: {},
          async execute(_args, context) {
            const response = await native.request<Record<string, unknown>>(
              "optimize",
              {},
              context.abort,
            );
            session.invalidateRecall();
            return result("Optimized memory", response, response);
          },
        }),
        memory_doctor: tool({
          description:
            "Diagnose state compatibility, index health, retention, code anchors, and model cache.",
          args: {
            deep: tool.schema
              .boolean()
              .default(false)
              .describe("Hash all code anchors to detect staleness."),
          },
          async execute(args, context) {
            const response = await native.request<Record<string, unknown>>(
              "doctor",
              args,
              context.abort,
            );
            return result("Memory doctor", response, response);
          },
        }),
        memory_status: tool({
          description:
            "Inspect the current project's native memory backend, collection, embedding model, indexes, and document count.",
          args: {},
          async execute(_args, context) {
            const response = await native.request<Record<string, unknown>>(
              "status",
              {},
              context.abort,
            );
            return result("Memory status", response, response);
          },
        }),
      },
    };
  };
}

interface ResolvedMemoryPluginOptions {
  warmup: boolean;
  automaticRecall: boolean;
  automaticCapture: boolean;
  sharedSync: boolean;
  feedbackTracking: boolean;
  minScore: number;
}

export function resolveMemoryPluginOptions(
  options: MemoryPluginOptions,
): ResolvedMemoryPluginOptions {
  const minScore = options.minScore ?? envNumber("OPENCODE_MEMORY_MIN_SCORE", 0.42);
  if (!Number.isFinite(minScore) || minScore < 0 || minScore > 1) {
    throw new Error("memory minScore must be between 0 and 1");
  }
  return {
    warmup: options.warmup ?? envBoolean("OPENCODE_MEMORY_WARMUP", true),
    automaticRecall: options.automaticRecall ?? envBoolean("OPENCODE_MEMORY_AUTO_RECALL", true),
    automaticCapture: options.automaticCapture ?? envBoolean("OPENCODE_MEMORY_AUTO_CAPTURE", true),
    sharedSync: options.sharedSync ?? envBoolean("OPENCODE_MEMORY_SHARED_SYNC", true),
    feedbackTracking:
      options.feedbackTracking ?? envBoolean("OPENCODE_MEMORY_FEEDBACK_TRACKING", true),
    minScore,
  };
}

function envBoolean(name: string, fallback: boolean): boolean {
  const value = process.env[name];
  if (value === undefined || value === "") return fallback;
  if (["1", "true", "yes", "on"].includes(value.toLowerCase())) return true;
  if (["0", "false", "no", "off"].includes(value.toLowerCase())) return false;
  throw new Error(`${name} must be a boolean`);
}

function envNumber(name: string, fallback: number): number {
  const value = process.env[name];
  if (value === undefined || value === "") return fallback;
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) throw new Error(`${name} must be a finite number`);
  return parsed;
}

function result(title: string, value: unknown, metadata: Record<string, unknown>): ToolResult {
  return {
    title,
    output: JSON.stringify(value, null, 2),
    metadata,
  };
}
