import { describe, expect, test } from "bun:test";
import type { CaptureResponse } from "./contracts.js";
import {
  captureWithOutcomeReconciliation,
  type ReconciledCaptureResponse,
} from "./capture-reconciliation.js";
import { DaemonOutcomeUnknownError, type NativeMemoryRequester } from "./daemon-client.js";
import type { MemoryMethod } from "./protocol.js";

describe("automatic capture reconciliation", () => {
  test("does not replay a capture that returned normally", async () => {
    const response = storedResponse();
    const native = captureRequester([response]);

    const result = await captureWithOutcomeReconciliation(native, { candidate: "memory" });

    expect(result).toEqual({ response, reconciled: false, storedOrDuplicate: true });
    expect(native.calls).toBe(1);
  });

  test("replays once and treats duplicate as a committed first attempt", async () => {
    const duplicate: CaptureResponse = {
      decision: { outcome: "skip", reason: "duplicate" },
    };
    const native = captureRequester([
      new DaemonOutcomeUnknownError("response lost", "call-1"),
      duplicate,
    ]);

    const result = await captureWithOutcomeReconciliation(native, { candidate: "memory" });

    expect(result).toEqual({ response: duplicate, reconciled: true, storedOrDuplicate: true });
    expect(native.calls).toBe(2);
  });

  test("does not replay definite capture failures", async () => {
    const failure = new Error("validation failed");
    const native = captureRequester([failure]);

    await expect(captureWithOutcomeReconciliation(native, {})).rejects.toThrow("validation failed");
    expect(native.calls).toBe(1);
  });
});

function captureRequester(
  outcomes: Array<CaptureResponse | Error>,
): NativeMemoryRequester & { calls: number } {
  return {
    calls: 0,
    async request<T>(method: MemoryMethod): Promise<T> {
      expect(method).toBe("capture");
      const outcome = outcomes[this.calls++];
      if (outcome instanceof Error) throw outcome;
      if (!outcome) throw new Error("missing test outcome");
      return outcome as T;
    },
  };
}

function storedResponse(): CaptureResponse {
  return {
    decision: { outcome: "accept" },
    stored: {
      id: "mem_1",
      inserted: true,
      content_hash: "hash",
      updated_at_ms: 1,
      scope: "project",
    },
  };
}
