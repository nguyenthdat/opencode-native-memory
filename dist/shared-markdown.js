import { createHash } from "node:crypto";
import { lstat, readFile, readdir, realpath, rename, stat, writeFile, mkdir, } from "node:fs/promises";
import { isAbsolute, relative, resolve, sep } from "node:path";
import YAML from "yaml";
import { MEMORY_KINDS } from "./contracts.js";
export const SHARED_MEMORY_RELATIVE_DIR = ".opencode/memory";
const MAX_SHARED_FILES = 200;
const MAX_SHARED_FILE_BYTES = 64 * 1_024;
export async function loadSharedMemories(worktree) {
    const directory = resolve(worktree, SHARED_MEMORY_RELATIVE_DIR);
    const canonicalRoot = await realpath(worktree);
    let canonicalDirectory;
    try {
        const directoryInfo = await lstat(directory);
        if (directoryInfo.isSymbolicLink() || !directoryInfo.isDirectory()) {
            throw new Error("Shared memory directory must be a real directory, not a symlink");
        }
        canonicalDirectory = await realpath(directory);
        if (!isPathWithin(canonicalRoot, canonicalDirectory)) {
            throw new Error("Shared memory directory escaped the project worktree");
        }
    }
    catch (error) {
        if (isNodeError(error) && error.code === "ENOENT") {
            return { records: [], signature: createHash("sha256").digest("hex"), errors: [] };
        }
        throw error;
    }
    const errors = [];
    const files = await collectSharedMemoryFiles(canonicalDirectory, canonicalDirectory, "", errors);
    files.sort((left, right) => left.source.localeCompare(right.source));
    if (files.length > MAX_SHARED_FILES) {
        throw new Error(`At most ${MAX_SHARED_FILES} shared memory files are allowed`);
    }
    const hash = createHash("sha256");
    const records = [];
    for (const file of files) {
        hash.update(file.source).update("\0");
        try {
            const linkInfo = await lstat(file.path);
            if (linkInfo.isSymbolicLink() || !linkInfo.isFile()) {
                throw new Error(`Shared memory must be a regular file: ${file.source}`);
            }
            const canonicalPath = await realpath(file.path);
            if (!isPathWithin(canonicalDirectory, canonicalPath)) {
                throw new Error(`Shared memory file escaped its directory: ${file.source}`);
            }
            const info = await stat(canonicalPath);
            if (info.size > MAX_SHARED_FILE_BYTES) {
                throw new Error(`Shared memory file exceeds ${MAX_SHARED_FILE_BYTES} bytes: ${file.source}`);
            }
            const bytes = await readFile(canonicalPath);
            if (bytes.byteLength > MAX_SHARED_FILE_BYTES) {
                throw new Error(`Shared memory file exceeds ${MAX_SHARED_FILE_BYTES} bytes: ${file.source}`);
            }
            const content = bytes.toString("utf8");
            hash.update(content);
            records.push(parseSharedMemory(file.source, content));
        }
        catch (error) {
            const message = errorMessage(error);
            hash.update(`!error:${message}`);
            errors.push({ source: file.source, message });
        }
        hash.update("\0");
    }
    return { records, signature: hash.digest("hex"), errors };
}
async function collectSharedMemoryFiles(directory, canonicalRoot, relativeDirectory, errors) {
    const entries = await readdir(directory, { withFileTypes: true });
    entries.sort((left, right) => left.name.localeCompare(right.name));
    const files = [];
    for (const entry of entries) {
        const relativePath = relativeDirectory ? `${relativeDirectory}/${entry.name}` : entry.name;
        const source = `${SHARED_MEMORY_RELATIVE_DIR}/${relativePath}`;
        const path = resolve(directory, entry.name);
        const info = await lstat(path);
        if (info.isSymbolicLink()) {
            errors.push({ source, message: `Shared memory path must not be a symlink: ${source}` });
            continue;
        }
        if (info.isDirectory()) {
            const canonicalPath = await realpath(path);
            if (!isPathWithin(canonicalRoot, canonicalPath)) {
                throw new Error(`Shared memory directory escaped its root: ${source}`);
            }
            files.push(...(await collectSharedMemoryFiles(canonicalPath, canonicalRoot, relativePath, errors)));
            continue;
        }
        if (!entry.name.endsWith(".md"))
            continue;
        if (!info.isFile()) {
            errors.push({ source, message: `Shared memory must be a regular file: ${source}` });
            continue;
        }
        files.push({ path, source });
    }
    return files;
}
export function parseSharedMemory(source, input) {
    if (!input.startsWith("---\n")) {
        throw new Error(`Shared memory is missing YAML frontmatter: ${source}`);
    }
    const end = input.indexOf("\n---\n", 4);
    if (end < 0) {
        throw new Error(`Shared memory has malformed YAML frontmatter: ${source}`);
    }
    const metadata = YAML.parse(input.slice(4, end));
    const content = input.slice(end + 5).trim();
    if (!isObject(metadata))
        throw new Error(`Invalid shared memory: ${source}`);
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
    if (metadata.schema_version !== 1 ||
        typeof metadata.title !== "string" ||
        metadata.title.length === 0 ||
        metadata.title.length > 160 ||
        !MEMORY_KINDS.includes(metadata.kind) ||
        typeof metadata.importance !== "number" ||
        metadata.importance < 0 ||
        metadata.importance > 1 ||
        !isStringArray(metadata.tags, 12, 64) ||
        !isStringArray(metadata.code_paths, 12, 512) ||
        content.length === 0 ||
        content.length > 6_000) {
        throw new Error(`Shared memory fields are invalid: ${source}`);
    }
    return {
        source,
        title: metadata.title,
        content,
        kind: metadata.kind,
        importance: metadata.importance,
        tags: metadata.tags,
        code_paths: metadata.code_paths,
    };
}
export async function writeSharedMemory(worktree, memory) {
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
    }
    catch (error) {
        if (!isNodeError(error) || error.code !== "ENOENT")
            throw error;
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
export async function ensureRealDirectory(path) {
    try {
        const info = await lstat(path);
        if (info.isSymbolicLink() || !info.isDirectory()) {
            throw new Error(`Expected a real directory, not a symlink: ${path}`);
        }
    }
    catch (error) {
        if (!isNodeError(error) || error.code !== "ENOENT")
            throw error;
        await mkdir(path, { recursive: false, mode: 0o700 });
    }
}
export function isPathWithin(root, candidate) {
    const path = relative(root, candidate);
    return path === "" || (path !== ".." && !path.startsWith(`..${sep}`) && !isAbsolute(path));
}
function isObject(value) {
    return typeof value === "object" && value !== null && !Array.isArray(value);
}
function isStringArray(value, maxItems, maxLength) {
    return (Array.isArray(value) &&
        value.length <= maxItems &&
        value.every((item) => typeof item === "string" && item.length > 0 && item.length <= maxLength));
}
function isNodeError(error) {
    return error instanceof Error && "code" in error;
}
function errorMessage(error) {
    return error instanceof Error ? error.message : String(error);
}
//# sourceMappingURL=shared-markdown.js.map