import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createMemoryPlugin } from "../../dist/index.js";

const projectRoot = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(projectRoot, "../..");

process.env.OPENCODE_MEMORY_DATA_DIR ??= resolve(projectRoot, ".memory-data");

export default createMemoryPlugin({
  root: repositoryRoot,
  projectRoot,
  warmup: process.env.OPENCODE_MEMORY_DEMO_WARMUP !== "false",
  automaticRecall: true,
  automaticCapture: true,
  sharedSync: true,
  feedbackTracking: true,
  minScore: 0.35,
});
