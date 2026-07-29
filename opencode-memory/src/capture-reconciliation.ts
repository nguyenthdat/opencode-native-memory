import type { CaptureResponse } from "./contracts.js";
import type { NativeMemoryRequester } from "./daemon-client.js";
import { isOutcomeUnknown } from "./outcome-reconciliation.js";

export interface ReconciledCaptureResponse {
  response: CaptureResponse;
  reconciled: boolean;
  storedOrDuplicate: boolean;
}

export async function captureWithOutcomeReconciliation(
  native: NativeMemoryRequester,
  request: unknown,
): Promise<ReconciledCaptureResponse> {
  try {
    const response = await native.request<CaptureResponse>("capture", request);
    return {
      response,
      reconciled: false,
      storedOrDuplicate: response.stored !== undefined,
    };
  } catch (error) {
    if (!isOutcomeUnknown(error)) throw error;
    const response = await native.request<CaptureResponse>("capture", request);
    return {
      response,
      reconciled: true,
      storedOrDuplicate:
        response.stored !== undefined ||
        (response.decision.outcome === "skip" && response.decision.reason === "duplicate"),
    };
  }
}
