import { randomUUID } from "node:crypto";
export const BACKGROUND_JOB_ID_PATTERN = /^job_[0-9a-f]{32}$/;
const DEFAULT_CAPACITY = 32;
const DEFAULT_TERMINAL_RETENTION_MS = 30 * 60_000;
export class BackgroundJobQueue {
    jobs = new Map();
    pending = [];
    capacity;
    terminalRetentionMs;
    now;
    createID;
    activeTask;
    activeController;
    disposed = false;
    constructor(options = {}) {
        this.capacity = options.capacity ?? DEFAULT_CAPACITY;
        this.terminalRetentionMs = options.terminalRetentionMs ?? DEFAULT_TERMINAL_RETENTION_MS;
        this.now = options.now ?? Date.now;
        this.createID = options.createID ?? (() => `job_${randomUUID().replaceAll("-", "")}`);
        if (!Number.isInteger(this.capacity) || this.capacity < 1) {
            throw new Error("Background job queue capacity must be a positive integer");
        }
        if (!Number.isInteger(this.terminalRetentionMs) || this.terminalRetentionMs < 0) {
            throw new Error("Background job terminal retention must be a non-negative integer");
        }
    }
    enqueue(input, execute) {
        this.ensureActive();
        this.prune();
        this.evictTerminalIfNeeded();
        if (this.jobs.size >= this.capacity) {
            throw new Error("Too many active memory ingestion jobs");
        }
        const job_id = this.nextID();
        const created_at_ms = this.now();
        const job = {
            job_id,
            input,
            created_at_ms,
            status: "queued",
        };
        this.jobs.set(job_id, job);
        this.pending.push({ job_id, input, created_at_ms, execute, controller: undefined });
        this.startDrain();
        return job;
    }
    get(jobID) {
        this.prune();
        return this.jobs.get(jobID);
    }
    list(jobIDs) {
        this.prune();
        if (jobIDs) {
            return jobIDs.map((jobID) => {
                const job = this.jobs.get(jobID);
                if (!job)
                    throw new Error(`Unknown or expired background job: ${jobID}`);
                return job;
            });
        }
        return [...this.jobs.values()].sort((left, right) => right.created_at_ms - left.created_at_ms);
    }
    async dispose() {
        if (this.disposed) {
            if (this.activeTask)
                await this.activeTask;
            return;
        }
        this.disposed = true;
        this.pending.length = 0;
        this.activeController?.abort();
        if (this.activeTask)
            await this.activeTask;
        this.jobs.clear();
    }
    ensureActive() {
        if (this.disposed)
            throw new Error("Background job queue is disposed");
    }
    nextID() {
        for (let attempt = 0; attempt < 10; attempt += 1) {
            const jobID = this.createID();
            if (!this.jobs.has(jobID))
                return jobID;
        }
        throw new Error("Could not allocate a unique background job ID");
    }
    prune() {
        const expiresBefore = this.now() - this.terminalRetentionMs;
        for (const [jobID, job] of this.jobs) {
            if ((job.status === "succeeded" || job.status === "failed") &&
                job.completed_at_ms <= expiresBefore) {
                this.jobs.delete(jobID);
            }
        }
    }
    evictTerminalIfNeeded() {
        if (this.jobs.size < this.capacity)
            return;
        let oldest;
        let oldestCreated = Number.POSITIVE_INFINITY;
        for (const [jobID, job] of this.jobs) {
            if ((job.status === "succeeded" || job.status === "failed") &&
                job.created_at_ms < oldestCreated) {
                oldest = jobID;
                oldestCreated = job.created_at_ms;
            }
        }
        if (oldest)
            this.jobs.delete(oldest);
    }
    startDrain() {
        if (this.activeTask)
            return;
        this.activeTask = this.drain().finally(() => {
            this.activeTask = undefined;
            if (!this.disposed && this.pending.length > 0)
                this.startDrain();
        });
    }
    async drain() {
        while (!this.disposed) {
            const pending = this.pending.shift();
            if (!pending)
                return;
            const queued = this.jobs.get(pending.job_id);
            if (!queued)
                continue;
            pending.controller = new AbortController();
            this.activeController = pending.controller;
            this.jobs.set(pending.job_id, {
                job_id: pending.job_id,
                input: pending.input,
                created_at_ms: pending.created_at_ms,
                status: "running",
            });
            try {
                const result = await pending.execute(pending.controller.signal);
                if (!this.disposed) {
                    this.jobs.set(pending.job_id, {
                        job_id: pending.job_id,
                        input: pending.input,
                        created_at_ms: pending.created_at_ms,
                        completed_at_ms: this.now(),
                        status: "succeeded",
                        result,
                    });
                }
            }
            catch (error) {
                if (!this.disposed) {
                    this.jobs.set(pending.job_id, {
                        job_id: pending.job_id,
                        input: pending.input,
                        created_at_ms: pending.created_at_ms,
                        completed_at_ms: this.now(),
                        status: "failed",
                        error: error instanceof Error ? error.message : String(error),
                    });
                }
            }
            finally {
                pending.controller = undefined;
                this.activeController = undefined;
            }
        }
    }
}
//# sourceMappingURL=background-jobs.js.map