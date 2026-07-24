import { fileURLToPath } from "node:url";
import { basename, dirname, resolve } from "node:path";
import { createMemoryPlugin } from "./plugin.js";
const moduleDirectory = dirname(fileURLToPath(import.meta.url));
const packageRoot = basename(moduleDirectory) === "dist"
    ? resolve(moduleDirectory, "..")
    : resolve(moduleDirectory, "../..");
const memoryPlugin = {
    id: "@nguyenthdat/opencode-memory",
    server: createMemoryPlugin({ root: packageRoot }),
};
export default memoryPlugin;
//# sourceMappingURL=server.js.map