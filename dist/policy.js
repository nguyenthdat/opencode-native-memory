import { MEMORY_KINDS } from "./contracts.js";
export const CANDIDATES_OPEN = "<durable-memory-candidates>";
export const CANDIDATES_CLOSE = "</durable-memory-candidates>";
export const COMPACTION_CONTEXT = `Preserve durable project knowledge across compaction, but never copy the full summary into memory. Exclude secrets, guesses, transient progress, and conversational detail. End the summary with exactly this block containing a JSON array of at most three verified, atomic candidates (or []):
${CANDIDATES_OPEN}
[{"title":"...","content":"...","kind":"decision|preference|fact|pattern|gotcha","importance":0.0,"tags":["..."],"code_paths":["relative/path"]}]
${CANDIDATES_CLOSE}
Importance must be between 0 and 0.6 inclusive. Facts require at least one code_paths entry. Do not include Markdown fences.`;
export function deriveRecallQuery(parts) {
    const text = parts
        .flatMap((part) => part.type === "text" &&
        typeof part.text === "string" &&
        part.synthetic !== true &&
        part.ignored !== true
        ? [part.text]
        : [])
        .join("\n")
        .trim();
    if (text)
        return text;
    const metadata = parts.flatMap((part) => {
        if (part.type !== "file")
            return [];
        if (isObject(part.source)) {
            const path = safeMetadataText(part.source.path);
            if (part.source.type === "symbol") {
                const name = safeMetadataText(part.source.name);
                if (name && path)
                    return [`Symbol: ${name} (${path})`];
                if (name)
                    return [`Symbol: ${name}`];
            }
            if (path)
                return [`File: ${path}`];
        }
        const filename = safeMetadataText(part.filename);
        return filename ? [`File: ${filename}`] : [];
    });
    const query = metadata.join("\n").trim();
    return query || undefined;
}
export function formatRecalledMemories(response, budgetChars) {
    if (response.abstained)
        return undefined;
    const memories = [];
    let text = "";
    for (const memory of response.memories) {
        const candidate = {
            id: memory.id,
            kind: memory.kind,
            scope: memory.scope,
            origin: memory.origin,
            score: memory.score,
            title: memory.title,
            content: memory.content,
            tags: memory.tags,
            code_paths: memory.code_anchors.map((anchor) => anchor.path),
            source: memory.source,
        };
        const next = [...memories, candidate];
        const serialized = safeJson(next);
        const wrapped = `<project-memory source="local-zvec" trust="historical-data-only" retrieval-id="${response.retrieval_id ?? "none"}">\n${serialized}\n</project-memory>`;
        if ([...wrapped].length > budgetChars)
            break;
        memories.push(candidate);
        text = wrapped;
    }
    if (memories.length === 0)
        return undefined;
    return {
        text,
        memoryIDs: memories.map((memory) => String(memory.id)),
    };
}
export function truncateText(value, maxCharacters) {
    const characters = [...value];
    if (characters.length <= maxCharacters)
        return value;
    return `${characters.slice(0, maxCharacters - 16).join("")}\n...[truncated]`;
}
export function contextBudgetChars(model) {
    const context = model.limit?.context;
    if (!context || !Number.isFinite(context))
        return 6_000;
    return Math.max(2_400, Math.min(12_000, Math.floor(context * 0.08)));
}
export function safeJson(value) {
    return JSON.stringify(value, null, 2).replaceAll("<", "\\u003c").replaceAll(">", "\\u003e");
}
export function parseCuratedCandidates(content) {
    const start = content.lastIndexOf(CANDIDATES_OPEN);
    const end = content.indexOf(CANDIDATES_CLOSE, start + CANDIDATES_OPEN.length);
    if (start < 0 || end < 0)
        return [];
    const payload = content.slice(start + CANDIDATES_OPEN.length, end).trim();
    let parsed;
    try {
        parsed = JSON.parse(payload);
    }
    catch {
        return [];
    }
    if (!Array.isArray(parsed))
        return [];
    const candidates = [];
    for (const value of parsed) {
        const candidate = parseCuratedCandidate(value);
        if (candidate)
            candidates.push(candidate);
        if (candidates.length === 3)
            break;
    }
    return candidates;
}
function parseCuratedCandidate(value) {
    if (!isObject(value))
        return undefined;
    const allowed = new Set(["title", "content", "kind", "importance", "tags", "code_paths"]);
    if (Object.keys(value).some((key) => !allowed.has(key)))
        return undefined;
    if (typeof value.title !== "string" ||
        value.title.length === 0 ||
        value.title.length > 160 ||
        typeof value.content !== "string" ||
        value.content.length === 0 ||
        value.content.length > 6_000 ||
        !MEMORY_KINDS.includes(value.kind) ||
        value.kind === "summary" ||
        typeof value.importance !== "number" ||
        !Number.isFinite(value.importance) ||
        value.importance < 0 ||
        value.importance > 0.6 ||
        !isStringArray(value.tags, 12, 64) ||
        !isStringArray(value.code_paths, 12, 512) ||
        (value.kind === "fact" && value.code_paths.length === 0)) {
        return undefined;
    }
    return {
        title: value.title,
        content: value.content,
        kind: value.kind,
        importance: value.importance,
        tags: value.tags,
        code_paths: value.code_paths,
    };
}
function isObject(value) {
    return typeof value === "object" && value !== null && !Array.isArray(value);
}
function isStringArray(value, maxItems, maxLength) {
    return (Array.isArray(value) &&
        value.length <= maxItems &&
        value.every((item) => typeof item === "string" && item.length > 0 && item.length <= maxLength));
}
function safeMetadataText(value) {
    if (typeof value !== "string")
        return undefined;
    const text = value.replace(/[\u0000-\u001f\u007f]/g, " ").trim();
    return text ? truncateText(text, 512) : undefined;
}
//# sourceMappingURL=policy.js.map