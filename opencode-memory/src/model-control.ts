import type {
  MemoryModelProfilesResponse,
  ModelSwitchCancelResponse,
  ModelSwitchResponse,
  ModelSwitchStatusResponse,
} from "./contracts.js";
import { DaemonOutcomeUnknownError } from "./daemon-client.js";
import type { NativeMemoryRequester } from "./daemon-client.js";

export interface ModelSwitchPreflightOptions {
  profile_id: string;
  allow_dense_downtime?: boolean;
  force_rebuild?: boolean;
  expected_active_profile_id?: string | undefined;
  expected_active_generation_id?: string | undefined;
  retain_previous?: boolean;
  target_generation_id?: string | undefined;
}

export interface ModelSwitchStartOptions extends ModelSwitchPreflightOptions {
  switch_id: string;
  dry_run?: boolean;
}

export async function listModelProfiles(
  native: NativeMemoryRequester,
  signal?: AbortSignal,
): Promise<MemoryModelProfilesResponse> {
  return await native.request<MemoryModelProfilesResponse>("model_profiles", {}, signal);
}

export async function preflightModelSwitch(
  native: NativeMemoryRequester,
  options: ModelSwitchPreflightOptions,
  signal?: AbortSignal,
): Promise<ModelSwitchResponse> {
  return await native.request<ModelSwitchResponse>(
    "model_switch",
    {
      target_profile_id: options.profile_id,
      allow_dense_downtime: options.allow_dense_downtime ?? false,
      force_rebuild: options.force_rebuild ?? false,
      expected_active_profile_id: options.expected_active_profile_id,
      ...(options.expected_active_generation_id === undefined
        ? {}
        : { expected_active_generation_id: options.expected_active_generation_id }),
      retain_previous: options.retain_previous ?? true,
      ...(options.target_generation_id === undefined
        ? {}
        : { target_generation_id: options.target_generation_id }),
      dry_run: true,
    },
    signal,
  );
}

export async function startModelSwitch(
  native: NativeMemoryRequester,
  options: ModelSwitchStartOptions,
  signal?: AbortSignal,
): Promise<ModelSwitchResponse> {
  return await native.request<ModelSwitchResponse>(
    "model_switch",
    {
      switch_id: options.switch_id,
      target_profile_id: options.profile_id,
      allow_dense_downtime: options.allow_dense_downtime ?? false,
      force_rebuild: options.force_rebuild ?? false,
      ...(options.expected_active_profile_id === undefined
        ? {}
        : { expected_active_profile_id: options.expected_active_profile_id }),
      ...(options.expected_active_generation_id === undefined
        ? {}
        : { expected_active_generation_id: options.expected_active_generation_id }),
      retain_previous: options.retain_previous ?? true,
      ...(options.target_generation_id === undefined
        ? {}
        : { target_generation_id: options.target_generation_id }),
      dry_run: options.dry_run ?? false,
    },
    signal,
  );
}

export async function getModelSwitchStatus(
  native: NativeMemoryRequester,
  switchID: string,
  signal?: AbortSignal,
): Promise<ModelSwitchStatusResponse> {
  return await native.request<ModelSwitchStatusResponse>(
    "model_switch_status",
    { switch_id: switchID },
    signal,
  );
}

export async function cancelModelSwitch(
  native: NativeMemoryRequester,
  switchID: string,
  signal?: AbortSignal,
): Promise<ModelSwitchCancelResponse> {
  try {
    return await native.request<ModelSwitchCancelResponse>(
      "model_switch_cancel",
      { switch_id: switchID },
      signal,
    );
  } catch (error) {
    if (!(error instanceof DaemonOutcomeUnknownError)) throw error;
    const status = await getModelSwitchStatus(native, switchID, signal);
    if (status.state === "succeeded") {
      return { switch_id: switchID, outcome: "already_committed" };
    }
    if (["cancelled", "failed"].includes(status.state)) {
      return { switch_id: switchID, outcome: "already_terminal" };
    }
    return await native.request<ModelSwitchCancelResponse>(
      "model_switch_cancel",
      { switch_id: switchID },
      signal,
    );
  }
}
