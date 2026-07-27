export declare const BACKGROUND_JOB_ID_PATTERN: RegExp;
export type BackgroundJobID = `job_${string}`;
interface BackgroundJobBase<TInput extends object> {
    readonly job_id: BackgroundJobID;
    readonly input: Readonly<TInput>;
    readonly created_at_ms: number;
}
export type BackgroundJob<TInput extends object, TResult> = (BackgroundJobBase<TInput> & {
    readonly status: "queued";
}) | (BackgroundJobBase<TInput> & {
    readonly status: "running";
}) | (BackgroundJobBase<TInput> & {
    readonly status: "succeeded";
    readonly completed_at_ms: number;
    readonly result: TResult;
}) | (BackgroundJobBase<TInput> & {
    readonly status: "failed";
    readonly completed_at_ms: number;
    readonly error: string;
});
export interface BackgroundJobQueueOptions {
    readonly capacity?: number;
    readonly terminalRetentionMs?: number;
    readonly now?: () => number;
    readonly createID?: () => BackgroundJobID;
}
export declare class BackgroundJobQueue<TInput extends object, TResult> {
    private readonly jobs;
    private readonly pending;
    private readonly capacity;
    private readonly terminalRetentionMs;
    private readonly now;
    private readonly createID;
    private activeTask;
    private activeController;
    private disposed;
    constructor(options?: BackgroundJobQueueOptions);
    enqueue(input: Readonly<TInput>, execute: (signal: AbortSignal) => Promise<TResult>): BackgroundJob<TInput, TResult>;
    get(jobID: string): BackgroundJob<TInput, TResult> | undefined;
    list(jobIDs?: readonly string[]): BackgroundJob<TInput, TResult>[];
    dispose(): Promise<void>;
    private ensureActive;
    private nextID;
    private prune;
    private evictTerminalIfNeeded;
    private startDrain;
    private drain;
}
export {};
//# sourceMappingURL=background-jobs.d.ts.map