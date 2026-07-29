import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
const output = await mkdtemp(join(tmpdir(), "opencode-memory-proto-"));
const generatedFiles = [
  "opencode/memory/v1/memory_pb.ts",
  "opencode/memory/model/v1/model_pb.ts",
  "opencode/memory/daemon/v1/daemon_pb.ts",
] as const;

try {
  const protoc = Bun.spawn(
    [
      "protoc",
      `--plugin=protoc-gen-es=${join(root, "node_modules/.bin/protoc-gen-es")}`,
      `--es_out=${output}`,
      "--es_opt=target=ts,import_extension=js",
      "-I",
      join(root, "schema"),
      join(root, "schema/opencode/memory/v1/memory.proto"),
      join(root, "schema/opencode/memory/model/v1/model.proto"),
      join(root, "schema/opencode/memory/daemon/v1/daemon.proto"),
    ],
    { cwd: root, stdout: "inherit", stderr: "inherit" },
  );
  const exitCode = await protoc.exited;
  if (exitCode !== 0) throw new Error(`protoc exited with status ${exitCode}`);

  for (const relative of generatedFiles) {
    const [expected, actual] = await Promise.all([
      readFile(join(output, relative)),
      readFile(join(root, "opencode-memory/src/generated", relative)),
    ]);
    if (!expected.equals(actual)) {
      throw new Error(`Generated protocol is stale: ${relative}`);
    }
  }
  console.log(`Verified ${generatedFiles.length} generated Protobuf files`);
} finally {
  await rm(output, { recursive: true, force: true });
}
