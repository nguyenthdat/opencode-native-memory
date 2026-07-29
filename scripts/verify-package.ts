export {};

const process = Bun.spawn(["npm", "pack", "--dry-run", "--json"], {
  stdout: "pipe",
  stderr: "pipe",
});
const [stdout, stderr, exitCode] = await Promise.all([
  new Response(process.stdout).text(),
  new Response(process.stderr).text(),
  process.exited,
]);
if (exitCode !== 0) throw new Error(stderr);
const [pack] = JSON.parse(stdout) as Array<{
  files: Array<{ path: string }>;
}>;
if (!pack) throw new Error("npm pack returned no package metadata");
const files = new Set(pack.files.map((entry) => entry.path));
const required = [
  "dist/index.js",
  "dist/index.d.ts",
  "dist/server.js",
  "dist/server.d.ts",
  "dist/tui.js",
  "dist/tui.d.ts",
  "dist/generated/opencode/memory/v1/memory_pb.js",
  "dist/generated/opencode/memory/model/v1/model_pb.js",
  "dist/generated/opencode/memory/daemon/v1/daemon_pb.js",
  "rules/native-memory.md",
  "LICENSE",
  "THIRD_PARTY_NOTICES.md",
  "notices/ZVEC_NOTICE",
];
const missing = required.filter((file) => !files.has(file));
if (missing.length > 0) {
  throw new Error(`npm package is missing: ${missing.join(", ")}`);
}
const forbidden = [...files].filter(
  (file) =>
    file.startsWith("src/") ||
    file.startsWith("opencode-memory/src/") ||
    file.startsWith("_workspace/") ||
    file.startsWith(".qdrant/") ||
    file.includes(".env"),
);
if (forbidden.length > 0) {
  throw new Error(`npm package contains forbidden files: ${forbidden.join(", ")}`);
}
const allowedExact = new Set([
  "package.json",
  "README.md",
  "LICENSE",
  "THIRD_PARTY_NOTICES.md",
  "notices/ZVEC_NOTICE",
  "rules/native-memory.md",
]);
const unexpected = [...files].filter(
  (file) => !file.startsWith("dist/") && !allowedExact.has(file),
);
if (unexpected.length > 0) {
  throw new Error(`npm package contains files outside the allowlist: ${unexpected.join(", ")}`);
}
const instructions = await Bun.file("rules/native-memory.md").text();
if (!instructions.includes("<!-- opencode-memory-instructions:v1 -->")) {
  throw new Error("rules/native-memory.md is missing the managed instruction marker");
}
const manifest = (await Bun.file("package.json").json()) as {
  name?: string;
  exports?: Record<string, { types?: string; import?: string; default?: string }>;
};
const serverExport = manifest.exports?.["./server"];
if (
  serverExport?.types !== "./dist/server.d.ts" ||
  serverExport.import !== "./dist/server.js" ||
  serverExport.default !== "./dist/server.js"
) {
  throw new Error("package.json must expose the dedicated ./server plugin entrypoint");
}
const tuiExport = manifest.exports?.["./tui"];
if (
  tuiExport?.types !== "./dist/tui.d.ts" ||
  tuiExport.import !== "./dist/tui.js" ||
  tuiExport.default !== "./dist/tui.js"
) {
  throw new Error("package.json must expose the dedicated ./tui plugin entrypoint");
}
const serverModule = (await import("../dist/server.js")) as {
  default?: { id?: string; server?: unknown };
};
const serverPlugin = serverModule.default;
if (
  !serverPlugin ||
  serverPlugin.id !== manifest.name ||
  typeof serverPlugin.server !== "function"
) {
  throw new Error("dist/server.js must default-export an OpenCode server plugin module");
}
const tuiEntrypoint = "../dist/tui.js";
const tuiModule = (await import(tuiEntrypoint)) as {
  default?: { id?: string; tui?: unknown };
};
const tuiPlugin = tuiModule.default;
if (!tuiPlugin || tuiPlugin.id !== manifest.name || typeof tuiPlugin.tui !== "function") {
  throw new Error("dist/tui.js must default-export an OpenCode TUI plugin module");
}
console.log(`npm package contains ${files.size} allowlisted files`);
