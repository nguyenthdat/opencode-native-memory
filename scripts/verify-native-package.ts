import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

const directory = resolve(process.argv[2] ?? "");
if (!process.argv[2]) throw new Error("native package directory is required");
const pkg = JSON.parse(await readFile(resolve(directory, "package.json"), "utf8")) as {
  name: string;
  os: string[];
};
const os = pkg.os[0];
if (os !== "darwin" && os !== "linux") {
  throw new Error(`${pkg.name} targets unsupported OS ${String(os)}`);
}

const packed = Bun.spawnSync(["npm", "pack", "--dry-run", "--json"], {
  cwd: directory,
  stdout: "pipe",
  stderr: "pipe",
});
if (!packed.success) throw new Error(packed.stderr.toString());
const [result] = JSON.parse(packed.stdout.toString()) as Array<{
  files: Array<{ path: string }>;
}>;
if (!result) throw new Error(`npm pack returned no metadata for ${pkg.name}`);
const files = new Set(result.files.map((entry) => entry.path));
const library = os === "darwin" ? "libzvec_c_api.dylib" : "libzvec_c_api.so";
const required = [
  "package.json",
  "bin/opencode-memory",
  `bin/memory-libs/${library}`,
  "LICENSE",
  "THIRD_PARTY_NOTICES.md",
  "notices/ZVEC_NOTICE",
];
const missing = required.filter((file) => !files.has(file));
if (missing.length > 0) throw new Error(`${pkg.name} is missing: ${missing.join(", ")}`);
const unexpected = [...files].filter((file) => !required.includes(file));
if (unexpected.length > 0) {
  throw new Error(`${pkg.name} contains unexpected files: ${unexpected.join(", ")}`);
}
console.log(`${pkg.name} contains the expected ${os} runtime payload`);
