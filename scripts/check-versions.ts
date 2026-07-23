import { readFile } from "node:fs/promises";
import { join } from "node:path";

const rootPackage = await readPackage("package.json");
const cargo = Bun.spawnSync(["cargo", "metadata", "--no-deps", "--format-version", "1"]);
if (!cargo.success) throw new Error(cargo.stderr.toString());
const metadata = JSON.parse(cargo.stdout.toString()) as {
  packages: Array<{ name: string; version: string }>;
};
const rustPackage = metadata.packages.find((entry) => entry.name === "opencode-memory");
if (!rustPackage) throw new Error("Cannot find opencode-memory in Cargo metadata");

const nativePackages = [
  "darwin-arm64",
  "darwin-x64",
  "linux-arm64-gnu",
  "linux-x64-gnu",
  "win32-x64-msvc",
];
const versions = new Map<string, string>([
  [rootPackage.name, rootPackage.version],
  [rustPackage.name, rustPackage.version],
]);
for (const directory of nativePackages) {
  const pkg = await readPackage(join("npm", directory, "package.json"));
  versions.set(pkg.name, pkg.version);
}
const mismatches = [...versions].filter(([, version]) => version !== rootPackage.version);
if (mismatches.length > 0) {
  throw new Error(
    `Release versions differ from ${rootPackage.version}: ${mismatches
      .map(([name, version]) => `${name}=${version}`)
      .join(", ")}`,
  );
}
console.log(`All package versions match ${rootPackage.version}`);

async function readPackage(path: string): Promise<{ name: string; version: string }> {
  return JSON.parse(await readFile(path, "utf8")) as {
    name: string;
    version: string;
  };
}
