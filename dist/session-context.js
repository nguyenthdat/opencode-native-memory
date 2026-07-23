import { FEEDBACK_EVENTS, WRITABLE_MEMORY_SCOPES } from "./contracts.js";
export class SessionContext {
    native;
    getSessionAPI;
    directory;
    latestQuery = new Map();
    recallCache = new Map();
    pendingRecall = new Map();
    sessionParents = new Map();
    sessionRoots = new Map();
    sessionAgents = new Map();
    warnings = new Set();
    constructor(native, getSessionAPI, directory) {
        this.native = native;
        this.getSessionAPI = getSessionAPI;
        this.directory = directory;
    }
    warnOnce = (error) => {
        const message = error instanceof Error ? error.message : String(error);
        if (this.warnings.has(message))
            return;
        this.warnings.add(message);
        console.warn(`[memory] ${message}`);
    };
    async resolveSessionRoot(sessionID) {
        const cached = this.sessionRoots.get(sessionID);
        if (cached)
            return cached;
        const chain = [];
        const seen = new Set();
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
                }
                catch {
                    complete = false;
                    break;
                }
                this.sessionParents.set(current, parent);
            }
            if (!parent)
                break;
            current = parent;
        }
        if (!complete)
            return sessionID;
        const root = current;
        for (const id of chain)
            this.sessionRoots.set(id, root);
        return root;
    }
    async scopeKey(scope, sessionID, agent) {
        if (scope === "session")
            return await this.resolveSessionRoot(sessionID);
        if (scope === "agent")
            return agent;
        return undefined;
    }
    async managementScopeKeys(sessionID, agent) {
        return {
            session_scope_key: await this.resolveSessionRoot(sessionID),
            agent_scope_key: agent,
        };
    }
    async recordFeedback(pending, event, memoryIDs = pending.memoryIDs) {
        try {
            await this.native.request("feedback", {
                retrieval_id: pending.retrievalID,
                event,
                memory_ids: memoryIDs,
            });
        }
        catch (error) {
            this.warnOnce(error);
        }
    }
    async closePendingRecall(sessionID, event) {
        const pending = this.pendingRecall.get(sessionID);
        if (!pending)
            return;
        this.pendingRecall.delete(sessionID);
        await this.recordFeedback(pending, event);
    }
}
//# sourceMappingURL=session-context.js.map