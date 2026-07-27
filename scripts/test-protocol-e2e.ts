import { probeNativeMemoryDaemon } from "../opencode-memory/src/daemon-client.js";
import { resolve } from "node:path";

process.env.OPENCODE_MEMORY_DAEMON_IDLE_SECONDS ??= "1";
process.env.OPENCODE_NATIVE_MEMORY_BIN ??= resolve(
  process.cwd(),
  "target",
  "debug",
  "opencode-memory",
);
const info = await probeNativeMemoryDaemon(process.cwd());
if (!info.capabilities.includes("project-actors")) {
  throw new Error("Daemon did not report project actor support");
}
if (!info.capabilities.includes("call-cancellation")) {
  throw new Error("Daemon did not report queued-call cancellation support");
}
await Bun.sleep(2_000);
try {
  process.kill(info.pid, 0);
  throw new Error("Idle daemon did not exit after the configured timeout");
} catch (error) {
  if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
}
console.log(`Protobuf daemon control round-trip passed for pid ${info.pid}`);
