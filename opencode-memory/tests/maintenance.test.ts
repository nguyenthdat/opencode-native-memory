import { describe, expect, test } from "bun:test";
import { DaemonOutcomeUnknownError, type NativeMemoryRequester } from "../src/daemon-client.js";
import { MemoryMaintenanceScheduler } from "../src/maintenance.js";
import { requestIdempotently } from "../src/outcome-reconciliation.js";
import type { MemoryMethod } from "../src/protocol.js";

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

  test("uses the same action threshold for index maintenance observations", async () => {
    const cases = [
      { completeness: 0.979, expectedCalls: 1 },
      { completeness: 0.98, expectedCalls: 1 },
      { completeness: 0.99, expectedCalls: 1 },
      { completeness: 1, expectedCalls: 0 },
    ];

    for (const testCase of cases) {
      let calls = 0;
      const scheduler = new MemoryMaintenanceScheduler(
        requester(async () => {
          calls += 1;
          return { optimized: true };
        }),
        { debounceMs: 50 },
      );
      scheduler.observeStatus({
        indexes: [{ name: "embedding", completeness: testCase.completeness }],
        pending_upsert_count: 0,
        pending_delete_count: 0,
      });
      await scheduler.flush();
      await scheduler.dispose();

      expect(calls).toBe(testCase.expectedCalls);
    }
  });

  test("schedules maintenance for pending durable journal entries", async () => {
    for (const field of ["pending_upsert_count", "pending_delete_count"] as const) {
      let calls = 0;
      const scheduler = new MemoryMaintenanceScheduler(
        requester(async () => {
          calls += 1;
          return { optimized: true };
        }),
        { debounceMs: 50 },
      );
      scheduler.observeStatus({
        indexes: [{ name: "embedding", completeness: 1 }],
        pending_upsert_count: field === "pending_upsert_count" ? 1 : 0,
        pending_delete_count: field === "pending_delete_count" ? 1 : 0,
      });
      await scheduler.flush();
      await scheduler.dispose();

      expect(calls).toBe(1);
    }
  });

  test("does not schedule maintenance for a not-ready backend", async () => {
    let calls = 0;
    const scheduler = new MemoryMaintenanceScheduler(
      requester(async () => {
        calls += 1;
        return { optimized: true };
      }),
      { debounceMs: 50 },
    );
    scheduler.observeStatus({
      ready: false,
      indexes: [{ name: "embedding", completeness: 0.1 }],
      pending_upsert_count: 1,
      pending_delete_count: 1,
    });
    await scheduler.flush();
    await scheduler.dispose();

    expect(calls).toBe(0);
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
