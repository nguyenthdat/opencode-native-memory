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
  private recallEpoch = 0;
  private readonly sessionRecallEpochs = new Map<string, number>();
  private readonly automaticRecallSearches = new Map<
    string,
    { key: string; promise: Promise<SearchResponse> }
  >();

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

  async openPendingRecall(
    sessionID: string,
    pending: PendingRecall,
    isCurrent: () => boolean = () => true,
  ): Promise<boolean> {
    while (this.pendingRecall.has(sessionID)) {
      await this.closePendingRecall(sessionID, "ignored");
      if (!isCurrent()) return false;
    }
    if (!isCurrent()) return false;
    this.pendingRecall.set(sessionID, pending);
    await this.recordFeedback(pending, "injected");
    if (isCurrent()) return true;
    if (this.pendingRecall.get(sessionID) === pending) {
      await this.closePendingRecall(sessionID, "ignored");
    }
    return false;
  }

  invalidateRecall(sessionID?: string): void {
    if (sessionID === undefined) {
      this.recallEpoch += 1;
      this.recallCache.clear();
      return;
    }
    this.sessionRecallEpochs.set(sessionID, (this.sessionRecallEpochs.get(sessionID) ?? 0) + 1);
    this.recallCache.delete(sessionID);
  }

  recallGeneration(sessionID: string): string {
    return `${this.recallEpoch}:${this.sessionRecallEpochs.get(sessionID) ?? 0}`;
  }

  async searchRecallOnce(
    sessionID: string,
    key: string,
    search: () => Promise<SearchResponse>,
  ): Promise<SearchResponse> {
    const current = this.automaticRecallSearches.get(sessionID);
    if (current?.key === key) return await current.promise;

    const promise = search().finally(() => {
      if (this.automaticRecallSearches.get(sessionID)?.promise === promise) {
        this.automaticRecallSearches.delete(sessionID);
      }
    });
    this.automaticRecallSearches.set(sessionID, { key, promise });
    return await promise;
  }
}
