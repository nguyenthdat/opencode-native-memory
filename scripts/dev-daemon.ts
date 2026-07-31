import { access, readFile, realpath, stat } from "node:fs/promises";
import { constants } from "node:fs";
import { dirname, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import {
  probeNativeMemoryDaemon,
  requestNativeMemoryDaemonDrain,
  resolveDaemonEndpoint,
} from "../opencode-memory/src/daemon-client.js";

const EXIT_TIMEOUT_MS = 15_000;
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const { binary, force } = parseArguments(process.argv.slice(2));
const candidate = await realpath(resolve(binary));
const metadata = await stat(candidate);
if (!metadata.isFile()) throw new Error(`Development daemon is not a file: ${candidate}`);
await access(candidate, constants.X_OK);

process.env.OPENCODE_NATIVE_MEMORY_BIN = candidate;

const current = await requestNativeMemoryDaemonDrain(root);
if (current) {
  if (current.outcome !== "accepted") {
    if (!force) {
      const retry = current.retryAfterMs > 0 ? ` Retry after ${current.retryAfterMs} ms.` : "";
      throw new Error(
        `Daemon pid ${current.daemon.pid} is ${current.outcome} and cannot drain safely.${retry} ` +
          "Close active OpenCode clients or run `just daemon-swap-force`.",
      );
    }
    console.warn(
      `Daemon pid ${current.daemon.pid} is ${current.outcome}; sending SIGTERM for an explicit dev swap`,
    );
    process.kill(current.daemon.pid, "SIGTERM");
  }
  await waitForProcessExit(current.daemon.pid);
}

const next = await probeNativeMemoryDaemon(root);
const packageVersion = await readPackageVersion();
if (next.daemonVersion !== packageVersion) {
  throw new Error(
    `Started daemon version ${next.daemonVersion}, but package.json declares ${packageVersion}`,
  );
}
if (current && next.daemonInstanceId === current.daemon.daemonInstanceId) {
  throw new Error("Daemon instance did not change during the development swap");
}

console.log(
  JSON.stringify(
    {
      endpoint: resolveDaemonEndpoint(),
      binary: candidate,
      previousPid: current?.daemon.pid ?? null,
      pid: next.pid,
      daemonInstanceId: next.daemonInstanceId,
      daemonVersion: next.daemonVersion,
    },
    null,
    2,
  ),
);

function parseArguments(args: string[]): { binary: string; force: boolean } {
  if (args[0] !== "swap") usage();
  let binary: string | undefined;
  let force = false;
  for (let index = 1; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === "--force") {
      force = true;
      continue;
    }
    if (argument === "--binary") {
      binary = args[index + 1];
      index += 1;
      if (!binary) usage();
      continue;
    }
    usage();
  }
  if (!binary) usage();
  return { binary, force };
}

function usage(): never {
  throw new Error("Usage: bun scripts/dev-daemon.ts swap --binary <path> [--force]");
}

async function waitForProcessExit(pid: number): Promise<void> {
  const deadline = Date.now() + EXIT_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (!processIsAlive(pid)) return;
    await delay(50);
  }
  throw new Error(`Timed out waiting for daemon pid ${pid} to exit`);
}

function processIsAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === "EPERM";
  }
}

async function readPackageVersion(): Promise<string> {
  const value = JSON.parse(await readFile(resolve(root, "package.json"), "utf8")) as {
    version?: unknown;
  };
  if (typeof value.version !== "string" || value.version.length === 0) {
    throw new Error("package.json does not declare a version");
  }
  return value.version;
}
