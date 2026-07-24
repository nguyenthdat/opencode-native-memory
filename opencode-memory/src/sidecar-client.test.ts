import { expect, test } from "bun:test";
import { resolveNativeMemoryBinary } from "./sidecar-client.js";

test("rejects Intel macOS as unsupported", () => {
  const platform = process.platform;
  const arch = process.arch;
  Object.defineProperty(process, "platform", { value: "darwin" });
  Object.defineProperty(process, "arch", { value: "x64" });

  try {
    expect(() => resolveNativeMemoryBinary(".")).toThrow(
      "Native memory supports only macOS arm64 and glibc Linux arm64/x64, not darwin-x64",
    );
  } finally {
    Object.defineProperty(process, "platform", { value: platform });
    Object.defineProperty(process, "arch", { value: arch });
  }
});
