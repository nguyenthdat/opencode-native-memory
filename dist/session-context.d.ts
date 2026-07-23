import type { PendingRecall, SearchResponse } from "./contracts.js";
import { FEEDBACK_EVENTS, WRITABLE_MEMORY_SCOPES } from "./contracts.js";
import type { NativeMemoryClient } from "./sidecar-client.js";
export declare class SessionContext {
    private readonly native;
    private readonly getSessionAPI;
    private readonly directory;
    readonly latestQuery: Map<string, {
        query: string;
        agent: string | undefined;
    }>;
    readonly recallCache: Map<string, {
        key: string;
        response: SearchResponse;
    }>;
    readonly pendingRecall: Map<string, PendingRecall>;
    readonly sessionParents: Map<string, string | undefined>;
    readonly sessionRoots: Map<string, string>;
    readonly sessionAgents: Map<string, string>;
    readonly warnings: Set<string>;
    private recallEpoch;
    private readonly sessionRecallEpochs;
    private readonly automaticRecallSearches;
    constructor(native: NativeMemoryClient, getSessionAPI: (path: {
        id: string;
    }, query: {
        directory: string;
    }) => Promise<{
        data: {
            parentID?: string | null;
        } | undefined;
    }>, directory: string);
    warnOnce: (error: unknown) => void;
    resolveSessionRoot(sessionID: string): Promise<string>;
    scopeKey(scope: (typeof WRITABLE_MEMORY_SCOPES)[number], sessionID: string, agent: string): Promise<string | undefined>;
    managementScopeKeys(sessionID: string, agent: string): Promise<{
        session_scope_key: string;
        agent_scope_key: string;
    }>;
    recordFeedback(pending: PendingRecall, event: "injected" | (typeof FEEDBACK_EVENTS)[number], memoryIDs?: string[]): Promise<void>;
    closePendingRecall(sessionID: string, event: "ignored" | "error"): Promise<void>;
    discardPendingRecall(sessionID: string): void;
    openPendingRecall(sessionID: string, pending: PendingRecall, isCurrent?: () => boolean): Promise<boolean>;
    invalidateRecall(sessionID?: string): void;
    recallGeneration(sessionID: string): string;
    searchRecallOnce(sessionID: string, key: string, search: () => Promise<SearchResponse>): Promise<SearchResponse>;
}
//# sourceMappingURL=session-context.d.ts.map