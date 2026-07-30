import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";

const runtimeRoot = await mkdtemp(resolve(tmpdir(), "opencode-memory-protocol-"));
process.env.TMPDIR = runtimeRoot;
process.env.XDG_RUNTIME_DIR = runtimeRoot;
process.env.OPENCODE_MEMORY_DAEMON_IDLE_SECONDS ??= "1";
process.env.OPENCODE_NATIVE_MEMORY_BIN ??= resolve(
  process.cwd(),
  "target",
  "debug",
  "opencode-memory",
);
try {
  const { probeNativeMemoryDaemon } = await import("../opencode-memory/src/daemon-client.js");
  const info = await probeNativeMemoryDaemon(process.cwd());
  if (!info.capabilities.includes("project-actors")) {
    throw new Error("Daemon did not report project actor support");
  }
  if (!info.capabilities.includes("call-cancellation")) {
    throw new Error("Daemon did not report queued-call cancellation support");
  }
  if (!info.capabilities.includes("knowledge-graph-v1")) {
    throw new Error("Daemon did not report knowledge graph support");
  }
  if (!info.capabilities.includes("graph-durable-extraction-jobs-v1")) {
    throw new Error("Daemon did not report durable graph extraction job support");
  }
  if (!info.capabilities.includes("daemon-periodic-optimize-v1")) {
    throw new Error("Daemon did not report periodic optimize support");
  }
  await Bun.sleep(2_000);
  try {
    process.kill(info.pid, 0);
    throw new Error("Idle daemon did not exit after the configured timeout");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
  }
  console.log(`Protobuf daemon control round-trip passed for pid ${info.pid}`);
} finally {
  await rm(runtimeRoot, { recursive: true, force: true });
}
