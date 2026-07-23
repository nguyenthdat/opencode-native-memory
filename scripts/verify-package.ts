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
  "dist/generated/opencode/memory/v1/memory_pb.js",
  "rules/flow.md",
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
  "rules/flow.md",
]);
const unexpected = [...files].filter(
  (file) => !file.startsWith("dist/") && !allowedExact.has(file),
);
if (unexpected.length > 0) {
  throw new Error(`npm package contains files outside the allowlist: ${unexpected.join(", ")}`);
}
const instructions = await Bun.file("rules/flow.md").text();
if (!instructions.includes("<!-- opencode-memory-instructions:v1 -->")) {
  throw new Error("rules/flow.md is missing the managed instruction marker");
}
console.log(`npm package contains ${files.size} allowlisted files`);
