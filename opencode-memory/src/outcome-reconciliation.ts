import { DaemonOutcomeUnknownError, type NativeMemoryRequester } from "./daemon-client.js";
import type { MemoryMethod } from "./protocol.js";

export type IdempotentMaintenanceMethod = "index_documents" | "sync_shared" | "optimize";

export async function requestIdempotently<T>(
  native: NativeMemoryRequester,
  method: IdempotentMaintenanceMethod,
  params: unknown,
  signal?: AbortSignal,
): Promise<{ response: T; reconciled: boolean }> {
  try {
    return {
      response: await native.request<T>(method, params, signal),
      reconciled: false,
    };
  } catch (error) {
    if (!isOutcomeUnknown(error) || signal?.aborted) throw error;
    return {
      response: await native.request<T>(method, params, signal),
      reconciled: true,
    };
  }
}

export function isOutcomeUnknown(error: unknown): boolean {
  return (
    error instanceof DaemonOutcomeUnknownError ||
    (typeof error === "object" &&
      error !== null &&
      "code" in error &&
      error.code === "OUTCOME_UNKNOWN")
  );
}

export function isIdempotentMaintenanceMethod(
  method: MemoryMethod,
): method is IdempotentMaintenanceMethod {
  return method === "index_documents" || method === "sync_shared" || method === "optimize";
}
