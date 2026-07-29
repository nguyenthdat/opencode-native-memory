import { describe, expect, test } from "bun:test";
import { DaemonOutcomeUnknownError, type NativeMemoryRequester } from "./daemon-client.js";
import { MemoryMaintenanceScheduler } from "./maintenance.js";
import { requestIdempotently } from "./outcome-reconciliation.js";
import type { MemoryMethod } from "./protocol.js";

describe("memory maintenance", () => {
  test("reconciles an idempotent maintenance mutation once", async () => {
    let calls = 0;
    const native = requester(async () => {
      calls += 1;
      if (calls === 1) throw new DaemonOutcomeUnknownError("lost", "call-1");
      return { optimized: true };
    });

    await expect(requestIdempotently(native, "index_documents", { force: false })).resolves.toEqual(
      {
        response: { optimized: true },
        reconciled: true,
      },
    );
    expect(calls).toBe(2);
  });

  test("coalesces maintenance triggers into one optimize request", async () => {
    let calls = 0;
    const native = requester(async () => {
      calls += 1;
      return { optimized: true };
    });
    const scheduler = new MemoryMaintenanceScheduler(native, { debounceMs: 50 });

    scheduler.schedule();
    scheduler.schedule();
    scheduler.observeStatus({
      indexes: [{ name: "embedding", completeness: 0.4 }],
      pending_upsert_count: 0,
      pending_delete_count: 0,
    });
    await scheduler.flush();
    await scheduler.dispose();

    expect(calls).toBe(1);
  });

  test("dispose cancels a scheduled optimization", async () => {
    let calls = 0;
    const native = requester(async () => {
      calls += 1;
      return { optimized: true };
    });
    const scheduler = new MemoryMaintenanceScheduler(native, { debounceMs: 50 });

    scheduler.schedule();
    await scheduler.dispose();
    expect(calls).toBe(0);
  });
});

function requester(
  implementation: (method: MemoryMethod, params: unknown) => Promise<unknown>,
): NativeMemoryRequester {
  return {
    async request<T>(method: MemoryMethod, params?: unknown): Promise<T> {
      return (await implementation(method, params)) as T;
    },
  };
}
