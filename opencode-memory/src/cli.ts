#!/usr/bin/env bun

import { randomUUID } from "node:crypto";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { acquireNativeMemoryClient } from "./daemon-client.js";
import { DaemonOutcomeUnknownError } from "./daemon-client.js";
import {
  cancelModelSwitch,
  getModelSwitchStatus,
  listModelProfiles,
  startModelSwitch,
} from "./model-control.js";

const packageRoot = resolve(fileURLToPath(new URL("../", import.meta.url)));
const args = process.argv.slice(2);

async function main(): Promise<void> {
  if (args[0] !== "model") usage();
  const command = args[1];
  if (!command) usage();
  if (!["profiles", "status", "cancel", "switch", "rollback"].includes(command)) usage();
  const options = parseOptions(command, args.slice(2));
  const projectRoot = resolve(options.value("--project-root") ?? process.cwd());
  const lease = await acquireNativeMemoryClient(packageRoot, projectRoot);
  try {
    switch (command) {
      case "profiles":
        print(await listModelProfiles(lease.client));
        return;
      case "status":
        print(await getModelSwitchStatus(lease.client, options.required("--switch-id")));
        return;
      case "cancel":
        print(await cancelModelSwitch(lease.client, options.required("--switch-id")));
        return;
      case "switch":
      case "rollback": {
        const profile = options.required("--profile");
        const switchID =
          options.value("--switch-id") ?? `switch_${randomUUID().replaceAll("-", "")}`;
        let response: Awaited<ReturnType<typeof startModelSwitch>>;
        try {
          response = await startModelSwitch(lease.client, {
            switch_id: switchID,
            profile_id: profile,
            allow_dense_downtime: options.has("--allow-dense-downtime"),
            force_rebuild: options.has("--force-rebuild"),
            dry_run: options.has("--dry-run"),
            retain_previous: !options.has("--discard-previous"),
            target_generation_id:
              command === "rollback"
                ? options.required("--generation")
                : options.value("--target-generation"),
          });
        } catch (error) {
          if (!(error instanceof DaemonOutcomeUnknownError)) throw error;
          let status: Awaited<ReturnType<typeof getModelSwitchStatus>>;
          try {
            status = await getModelSwitchStatus(lease.client, switchID);
          } catch {
            throw error;
          }
          print(status);
          if (["cancelled", "failed"].includes(status.state)) {
            throw new Error(`model switch ${switchID} ended in ${status.state}`);
          }
          if (options.has("--wait") && status.state !== "succeeded") {
            await waitForSwitch(lease.client, switchID);
          }
          return;
        }
        print(response);
        if (options.has("--wait") && !response.dry_run && response.switch_id) {
          await waitForSwitch(lease.client, response.switch_id);
        }
        return;
      }
      default:
        usage();
    }
  } finally {
    await lease.release();
  }
}

async function waitForSwitch(
  native: Parameters<typeof getModelSwitchStatus>[0],
  switchID: string,
): Promise<void> {
  for (;;) {
    const status = await getModelSwitchStatus(native, switchID);
    print(status);
    if (status.state === "succeeded") return;
    if (["cancelled", "failed"].includes(status.state)) {
      throw new Error(`model switch ${switchID} ended in ${status.state}`);
    }
    await Bun.sleep(1_000);
  }
}

class ParsedOptions {
  constructor(
    private readonly values: ReadonlyMap<string, string>,
    private readonly flags: ReadonlySet<string>,
  ) {}

  value(name: string): string | undefined {
    return this.values.get(name);
  }

  required(name: string): string {
    const value = this.value(name);
    if (!value) throw new Error(`${name} is required`);
    return value;
  }

  has(name: string): boolean {
    return this.flags.has(name);
  }
}

function parseOptions(command: string, input: string[]): ParsedOptions {
  const valueOptions = new Set([
    "--project-root",
    ...(command === "status" || command === "cancel" ? ["--switch-id"] : []),
    ...(command === "switch" || command === "rollback" ? ["--profile", "--switch-id"] : []),
    ...(command === "switch" ? ["--target-generation"] : []),
    ...(command === "rollback" ? ["--generation"] : []),
  ]);
  const flagOptions = new Set(
    command === "switch" || command === "rollback"
      ? ["--allow-dense-downtime", "--force-rebuild", "--discard-previous", "--dry-run", "--wait"]
      : [],
  );
  const values = new Map<string, string>();
  const flags = new Set<string>();
  for (let index = 0; index < input.length; index += 1) {
    const name = input[index];
    if (!name?.startsWith("--")) throw new Error(`unexpected argument: ${name ?? ""}`);
    if (valueOptions.has(name)) {
      if (values.has(name)) throw new Error(`${name} may only be provided once`);
      const value = input[index + 1];
      if (!value || value.startsWith("--")) throw new Error(`${name} requires a value`);
      values.set(name, value);
      index += 1;
    } else if (flagOptions.has(name)) {
      if (flags.has(name)) throw new Error(`${name} may only be provided once`);
      flags.add(name);
    } else {
      throw new Error(`unknown option for model ${command}: ${name}`);
    }
  }
  return new ParsedOptions(values, flags);
}

function print(value: unknown): void {
  process.stdout.write(`${JSON.stringify(value, null, 2)}\n`);
}

function usage(): never {
  throw new Error(
    "usage: opencode-memory model <profiles|switch|status|cancel|rollback> [--project-root PATH]",
  );
}

await main();
