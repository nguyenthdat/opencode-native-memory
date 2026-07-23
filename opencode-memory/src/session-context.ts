import type { PendingRecall, SearchResponse } from "./contracts.js";
import { FEEDBACK_EVENTS, WRITABLE_MEMORY_SCOPES } from "./contracts.js";
import type { NativeMemoryClient } from "./sidecar-client.js";

export class SessionContext {
  readonly latestQuery = new Map<string, { query: string; agent: string | undefined }>();
  readonly recallCache = new Map<string, { key: string; response: SearchResponse }>();
  readonly pendingRecall = new Map<string, PendingRecall>();
  readonly sessionParents = new Map<string, string | undefined>();
  readonly sessionRoots = new Map<string, string>();
  readonly sessionAgents = new Map<string, string>();
  readonly warnings = new Set<string>();

  constructor(
    private readonly native: NativeMemoryClient,
    private readonly getSessionAPI: (
      path: { id: string },
      query: { directory: string },
    ) => Promise<{ data: { parentID?: string | null } | undefined }>,
    private readonly directory: string,
  ) {}

  warnOnce = (error: unknown): void => {
    const message = error instanceof Error ? error.message : String(error);
    if (this.warnings.has(message)) return;
    this.warnings.add(message);
    console.warn(`[memory] ${message}`);
  };

  async resolveSessionRoot(sessionID: string): Promise<string> {
    const cached = this.sessionRoots.get(sessionID);
    if (cached) return cached;
    const chain: string[] = [];
    const seen = new Set<string>();
    let current = sessionID;
    let complete = true;
    for (let depth = 0; depth < 32 && !seen.has(current); depth += 1) {
      seen.add(current);
      chain.push(current);
      let parent = this.sessionParents.get(current);
      if (!this.sessionParents.has(current)) {
        try {
          const response = await this.getSessionAPI({ id: current }, { directory: this.directory });
          parent = response.data?.parentID ?? undefined;
        } catch {
          complete = false;
          break;
        }
        this.sessionParents.set(current, parent);
      }
      if (!parent) break;
      current = parent;
    }
    if (!complete) return sessionID;
    const root = current;
    for (const id of chain) this.sessionRoots.set(id, root);
    return root;
  }

  async scopeKey(
    scope: (typeof WRITABLE_MEMORY_SCOPES)[number],
    sessionID: string,
    agent: string,
  ): Promise<string | undefined> {
    if (scope === "session") return await this.resolveSessionRoot(sessionID);
    if (scope === "agent") return agent;
    return undefined;
  }

  async managementScopeKeys(
    sessionID: string,
    agent: string,
  ): Promise<{
    session_scope_key: string;
    agent_scope_key: string;
  }> {
    return {
      session_scope_key: await this.resolveSessionRoot(sessionID),
      agent_scope_key: agent,
    };
  }

  async recordFeedback(
    pending: PendingRecall,
    event: "injected" | (typeof FEEDBACK_EVENTS)[number],
    memoryIDs: string[] = pending.memoryIDs,
  ): Promise<void> {
    try {
      await this.native.request("feedback", {
        retrieval_id: pending.retrievalID,
        event,
        memory_ids: memoryIDs,
      });
    } catch (error) {
      this.warnOnce(error);
    }
  }

  async closePendingRecall(sessionID: string, event: "ignored" | "error"): Promise<void> {
    const pending = this.pendingRecall.get(sessionID);
    if (!pending) return;
    this.pendingRecall.delete(sessionID);
    await this.recordFeedback(pending, event);
  }
}
