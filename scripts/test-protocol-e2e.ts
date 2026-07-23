import { NativeMemoryClient } from "../opencode-memory/src/sidecar-client.js";

const client = new NativeMemoryClient(process.cwd(), process.cwd());
try {
  const response = await client.request<{ stopped: boolean }>("shutdown", {});
  if (response.stopped !== true) {
    throw new Error("Protobuf shutdown response did not confirm sidecar termination");
  }
  console.log("Protobuf TypeScript/Rust sidecar round-trip passed");
} finally {
  await client.dispose();
}
