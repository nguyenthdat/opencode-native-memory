import { describe, expect, test } from "bun:test";
import { BackgroundJobQueue } from "../src/background-jobs.js";
import { DaemonOutcomeUnknownError } from "../src/daemon-client.js";

const flush = async (): Promise<void> => {
  await Promise.resolve();
  await Promise.resolve();
};

function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
} {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

describe("background job queue", () => {
  test("returns immediately and processes jobs in FIFO order", async () => {
    const first = deferred<string>();
    const second = deferred<string>();
    let secondStarted = false;
    const queue = new BackgroundJobQueue<{ path: string }, string>();

    const firstJob = queue.enqueue({ path: "one.pdf" }, () => first.promise);
    const secondJob = queue.enqueue({ path: "two.pdf" }, () => {
      secondStarted = true;
      return second.promise;
    });

    expect(firstJob.status).toBe("queued");
    expect(secondJob.status).toBe("queued");
    await flush();
    expect(queue.get(firstJob.job_id)?.status).toBe("running");
    expect(queue.get(secondJob.job_id)?.status).toBe("queued");

    first.resolve("first");
    await flush();
    expect(secondStarted).toBe(true);
    expect(queue.get(firstJob.job_id)).toMatchObject({ status: "succeeded", result: "first" });

    second.resolve("second");
    await flush();
    expect(queue.get(secondJob.job_id)).toMatchObject({ status: "succeeded", result: "second" });
    await queue.dispose();
  });

  test("retains normalized failures for polling", async () => {
    const queue = new BackgroundJobQueue<Record<string, never>, string>();
    const job = queue.enqueue({}, async () => {
      throw new Error("document extraction failed");
    });
    await flush();
    expect(queue.get(job.job_id)).toMatchObject({
      status: "failed",
      error: "document extraction failed",
    });

    const nonErrorJob = queue.enqueue({}, async () => {
      throw "bad input";
    });
    await flush();
    expect(queue.get(nonErrorJob.job_id)).toMatchObject({ status: "failed", error: "bad input" });
    await queue.dispose();
  });

  test("preserves ambiguous mutation call IDs without reporting a definite failure", async () => {
    const queue = new BackgroundJobQueue<Record<string, never>, string>();
    const job = queue.enqueue({}, async () => {
      throw new DaemonOutcomeUnknownError("ingest outcome is unknown", "call-123");
    });
    await flush();

    expect(queue.get(job.job_id)).toMatchObject({
      status: "outcome_unknown",
      error: "ingest outcome is unknown",
      call_id: "call-123",
    });
    await queue.dispose();
  });

  test("expires terminal jobs and evicts the oldest terminal job at capacity", async () => {
    let now = 1_000;
    const queue = new BackgroundJobQueue<{ value: number }, number>({
      capacity: 2,
      terminalRetentionMs: 100,
      now: () => now,
    });
    const first = queue.enqueue({ value: 1 }, async () => 1);
    await flush();
    now += 1;
    const second = queue.enqueue({ value: 2 }, async () => 2);
    await flush();
    now += 1;
    const third = queue.enqueue({ value: 3 }, async () => 3);
    await flush();
    expect(queue.get(first.job_id)).toBeUndefined();
    expect(queue.get(second.job_id)).toBeDefined();
    expect(queue.get(third.job_id)).toBeDefined();
    now += 100;
    expect(queue.get(second.job_id)).toBeUndefined();
    expect(queue.get(third.job_id)).toBeUndefined();
    await queue.dispose();
  });

  test("aborts active work and rejects new jobs after disposal", async () => {
    const started = deferred<void>();
    let aborted = false;
    const queue = new BackgroundJobQueue<Record<string, never>, void>();
    queue.enqueue({}, (signal) => {
      started.resolve();
      return new Promise<void>((resolve) => {
        signal.addEventListener(
          "abort",
          () => {
            aborted = true;
            resolve();
          },
          { once: true },
        );
      });
    });
    await started.promise;
    await queue.dispose();
    expect(aborted).toBe(true);
    expect(queue.list()).toEqual([]);
    expect(() => queue.enqueue({}, async () => undefined)).toThrow("queue is disposed");
    await queue.dispose();
  });
});
