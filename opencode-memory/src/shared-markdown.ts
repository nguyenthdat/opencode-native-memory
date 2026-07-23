import { createHash } from "node:crypto";
import {
  lstat,
  readFile,
  readdir,
  realpath,
  rename,
  stat,
  writeFile,
  mkdir,
} from "node:fs/promises";
import { isAbsolute, relative, resolve } from "node:path";
import YAML from "yaml";
import type { MemoryRecord, SharedMemoryRecord } from "./contracts.js";
import { MEMORY_KINDS } from "./contracts.js";

export const SHARED_MEMORY_RELATIVE_DIR = ".opencode/memory";
const MAX_SHARED_FILES = 200;
const MAX_SHARED_FILE_BYTES = 64 * 1_024;

export async function loadSharedMemories(
  worktree: string,
): Promise<{ records: SharedMemoryRecord[]; signature: string }> {
  const directory = resolve(worktree, SHARED_MEMORY_RELATIVE_DIR);
  const canonicalRoot = await realpath(worktree);
  let entries;
  try {
    const directoryInfo = await lstat(directory);
    if (directoryInfo.isSymbolicLink() || !directoryInfo.isDirectory()) {
      throw new Error("Shared memory directory must be a real directory, not a symlink");
    }
    const canonicalDirectory = await realpath(directory);
    if (!isPathWithin(canonicalRoot, canonicalDirectory)) {
      throw new Error("Shared memory directory escaped the project worktree");
    }
    entries = await readdir(directory, { withFileTypes: true });
  } catch (error) {
    if (isNodeError(error) && error.code === "ENOENT") {
      return { records: [], signature: createHash("sha256").digest("hex") };
    }
    throw error;
  }
  const names = entries
    .filter((entry) => entry.isFile() && entry.name.endsWith(".md"))
    .map((entry) => entry.name)
    .sort();
  if (names.length > MAX_SHARED_FILES) {
    throw new Error(`At most ${MAX_SHARED_FILES} shared memory files are allowed`);
  }
  const hash = createHash("sha256");
  const records: SharedMemoryRecord[] = [];
  for (const name of names) {
    const path = resolve(directory, name);
    const linkInfo = await lstat(path);
    if (linkInfo.isSymbolicLink() || !linkInfo.isFile()) {
      throw new Error(`Shared memory must be a regular file: ${name}`);
    }
    const canonicalPath = await realpath(path);
    if (!isPathWithin(canonicalRoot, canonicalPath)) {
      throw new Error(`Shared memory file escaped the project worktree: ${name}`);
    }
    const info = await stat(canonicalPath);
    if (info.size > MAX_SHARED_FILE_BYTES) {
      throw new Error(`Shared memory file exceeds ${MAX_SHARED_FILE_BYTES} bytes: ${name}`);
    }
    const source = `${SHARED_MEMORY_RELATIVE_DIR}/${name}`;
    const content = await readFile(canonicalPath, "utf8");
    hash.update(source).update("\0").update(content).update("\0");
    records.push(parseSharedMemory(source, content));
  }
  return { records, signature: hash.digest("hex") };
}

export function parseSharedMemory(source: string, input: string): SharedMemoryRecord {
  if (!input.startsWith("---\n")) {
    throw new Error(`Shared memory is missing YAML frontmatter: ${source}`);
  }
  const end = input.indexOf("\n---\n", 4);
  if (end < 0) {
    throw new Error(`Shared memory has malformed YAML frontmatter: ${source}`);
  }
  const metadata: unknown = YAML.parse(input.slice(4, end));
  const content = input.slice(end + 5).trim();
  if (!isObject(metadata)) throw new Error(`Invalid shared memory: ${source}`);
  const allowed = new Set([
    "schema_version",
    "id",
    "title",
    "kind",
    "importance",
    "tags",
    "code_paths",
    "updated_at_ms",
  ]);
  if (Object.keys(metadata).some((key) => !allowed.has(key))) {
    throw new Error(`Shared memory has unknown fields: ${source}`);
  }
  if (
    metadata.schema_version !== 1 ||
    typeof metadata.title !== "string" ||
    metadata.title.length === 0 ||
    metadata.title.length > 160 ||
    !MEMORY_KINDS.includes(metadata.kind as (typeof MEMORY_KINDS)[number]) ||
    typeof metadata.importance !== "number" ||
    metadata.importance < 0 ||
    metadata.importance > 1 ||
    !isStringArray(metadata.tags, 12, 64) ||
    !isStringArray(metadata.code_paths, 12, 512) ||
    content.length === 0 ||
    content.length > 6_000
  ) {
    throw new Error(`Shared memory fields are invalid: ${source}`);
  }
  return {
    source,
    title: metadata.title,
    content,
    kind: metadata.kind as SharedMemoryRecord["kind"],
    importance: metadata.importance,
    tags: metadata.tags,
    code_paths: metadata.code_paths,
  };
}

export async function writeSharedMemory(worktree: string, memory: MemoryRecord): Promise<string> {
  const canonicalRoot = await realpath(worktree);
  const opencodeDirectory = resolve(canonicalRoot, ".opencode");
  await ensureRealDirectory(opencodeDirectory);
  const directory = resolve(opencodeDirectory, "memory");
  await ensureRealDirectory(directory);
  const canonicalDirectory = await realpath(directory);
  if (!isPathWithin(canonicalRoot, canonicalDirectory)) {
    throw new Error("Shared memory directory escaped the project worktree");
  }
  const destination = resolve(directory, `${memory.id}.md`);
  try {
    const destinationInfo = await lstat(destination);
    if (destinationInfo.isSymbolicLink() || !destinationInfo.isFile()) {
      throw new Error("Shared memory destination must be a regular file");
    }
  } catch (error) {
    if (!isNodeError(error) || error.code !== "ENOENT") throw error;
  }
  const relativePath = relative(canonicalRoot, destination).replaceAll("\\", "/");
  if (!relativePath.startsWith(`${SHARED_MEMORY_RELATIVE_DIR}/`)) {
    throw new Error("Shared memory destination escaped the project directory");
  }
  const frontmatter = YAML.stringify({
    schema_version: 1,
    id: memory.id,
    title: memory.title,
    kind: memory.kind,
    importance: memory.importance,
    tags: memory.tags,
    code_paths: memory.code_anchors.map((anchor) => anchor.path),
    updated_at_ms: memory.updated_at_ms,
  });
  const output = `---\n${frontmatter}---\n\n${memory.content.trim()}\n`;
  const temporary = `${destination}.tmp-${process.pid}-${Date.now()}`;
  await writeFile(temporary, output, { encoding: "utf8", flag: "wx", mode: 0o600 });
  await rename(temporary, destination);
  return relativePath;
}

export async function ensureRealDirectory(path: string): Promise<void> {
  try {
    const info = await lstat(path);
    if (info.isSymbolicLink() || !info.isDirectory()) {
      throw new Error(`Expected a real directory, not a symlink: ${path}`);
    }
  } catch (error) {
    if (!isNodeError(error) || error.code !== "ENOENT") throw error;
    await mkdir(path, { recursive: false, mode: 0o700 });
  }
}

export function isPathWithin(root: string, candidate: string): boolean {
  const path = relative(root, candidate);
  return path === "" || (!path.startsWith("..") && !isAbsolute(path));
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isStringArray(value: unknown, maxItems: number, maxLength: number): value is string[] {
  return (
    Array.isArray(value) &&
    value.length <= maxItems &&
    value.every((item) => typeof item === "string" && item.length > 0 && item.length <= maxLength)
  );
}

function isNodeError(error: unknown): error is NodeJS.ErrnoException {
  return error instanceof Error && "code" in error;
}
