import { mkdtemp, rm } from "node:fs/promises";
import { resolve } from "node:path";

const runtimeRoot = await mkdtemp("/tmp/om-proto-");
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
  const { probeNativeMemoryDaemon, requestNativeMemoryDaemonDrain } =
    await import("../opencode-memory/src/daemon-client.js");
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
  if (!info.capabilities.includes("durable-model-switch-v1")) {
    throw new Error("Daemon did not report durable model switch support");
  }
  await Bun.sleep(2_000);
  try {
    process.kill(info.pid, 0);
    throw new Error("Idle daemon did not exit after the configured timeout");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
  }

  const drainInfo = await probeNativeMemoryDaemon(process.cwd());
  await Bun.sleep(100);
  const drain = await requestNativeMemoryDaemonDrain(process.cwd());
  if (!drain || drain.daemon.pid !== drainInfo.pid || drain.outcome !== "accepted") {
    throw new Error("Daemon did not accept the control-plane drain request");
  }
  await Bun.sleep(2_000);
  try {
    process.kill(drainInfo.pid, 0);
    throw new Error("Drained daemon did not exit");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
  }
  console.log(
    `Protobuf daemon startup, idle exit, and drain passed for pids ${info.pid} and ${drainInfo.pid}`,
  );
} finally {
  await rm(runtimeRoot, { recursive: true, force: true });
}
