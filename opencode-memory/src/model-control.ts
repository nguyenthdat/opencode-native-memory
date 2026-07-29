import type { MemoryModelProfilesResponse, ModelSwitchResponse } from "./contracts.js";
import type { NativeMemoryRequester } from "./daemon-client.js";

export interface ModelSwitchPreflightOptions {
  profile_id: string;
  allow_dense_downtime?: boolean;
  force_rebuild?: boolean;
  expected_active_profile_id?: string | undefined;
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
      dry_run: true,
    },
    signal,
  );
}
