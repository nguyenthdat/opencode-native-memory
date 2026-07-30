import { expect, test } from "bun:test";
import * as sdkEntrypoint from "../src/index.js";
import memoryPlugin from "../src/server.js";

test("exports a dedicated OpenCode server plugin module", () => {
  expect(Object.values(sdkEntrypoint).some((value) => typeof value !== "function")).toBe(true);
  expect(sdkEntrypoint.default).toBe(memoryPlugin);
  expect(memoryPlugin.id).toBe("@nguyenthdat/opencode-memory");
  expect(typeof memoryPlugin.server).toBe("function");
});
