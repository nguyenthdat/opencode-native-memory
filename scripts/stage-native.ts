import { access, cp, mkdir, readdir, rm } from "node:fs/promises";
import { basename, join, resolve } from "node:path";

interface Options {
  target?: string;
  profile: "debug" | "release";
  destination: string;
  runtime: boolean;
}

async function main(): Promise<void> {
  const options = parseOptions(process.argv.slice(2));
  if (process.platform !== "darwin" && process.platform !== "linux") {
    throw new Error(`Native staging supports only macOS and Linux, not ${process.platform}`);
  }
  const targetPrefix = options.target ? join("target", options.target) : "target";
  const buildRoot = resolve(targetPrefix, options.profile);
  const executable = "opencode-memory";
  const sourceBinary = join(buildRoot, executable);
  const destination = resolve(options.destination);
  const binaryDirectory = join(destination, "bin");
  const libraryDirectory = join(binaryDirectory, "memory-libs");

  const libraryName = process.platform === "darwin" ? "libzvec_c_api.dylib" : "libzvec_c_api.so";
  const library = await findNativeLibrary(buildRoot, libraryName);
  if (!library) throw new Error(`Cannot find ${libraryName} below ${buildRoot}`);

  if (options.runtime) {
    const runtimeLibraryDirectory = join(destination, "memory-libs");
    await mkdir(runtimeLibraryDirectory, { recursive: true });
    await cp(library, join(runtimeLibraryDirectory, libraryName));
    return;
  }

  await rm(binaryDirectory, { recursive: true, force: true });
  await mkdir(libraryDirectory, { recursive: true });
  await cp(sourceBinary, join(binaryDirectory, executable));

  await cp(library, join(libraryDirectory, libraryName));
  await Promise.all(
    ["LICENSE", "THIRD_PARTY_NOTICES.md"].map((file) =>
      cp(resolve(file), join(destination, basename(file))),
    ),
  );
  await cp(resolve("notices"), join(destination, "notices"), {
    recursive: true,
  });
}

function parseOptions(args: string[]): Options {
  let target: string | undefined;
  let profile: Options["profile"] = "release";
  let destination: string | undefined;
  let runtime = false;
  for (let index = 0; index < args.length; index += 1) {
    const value = args[index];
    if (value === "--target") target = args[++index];
    else if (value === "--profile") profile = args[++index] as Options["profile"];
    else if (value === "--destination") destination = args[++index];
    else if (value === "--runtime") runtime = true;
    else throw new Error(`Unknown argument: ${value}`);
  }
  if (!destination) throw new Error("--destination is required");
  if (profile !== "debug" && profile !== "release") {
    throw new Error("--profile must be debug or release");
  }
  return { target, profile, destination, runtime };
}

async function findFile(root: string, name: string): Promise<string | undefined> {
  const entries = await readdir(root, { recursive: true, withFileTypes: true });
  return entries
    .filter((entry) => entry.isFile() && entry.name === name)
    .map((entry) => join(entry.parentPath, entry.name))
    .at(0);
}

async function findNativeLibrary(
  buildRoot: string,
  libraryName: string,
): Promise<string | undefined> {
  if (process.env.ZVEC_LIB_DIR) {
    const candidate = join(process.env.ZVEC_LIB_DIR, libraryName);
    if (
      await access(candidate)
        .then(() => true)
        .catch(() => false)
    )
      return candidate;
  }
  return await findFile(join(buildRoot, "build"), libraryName);
}

await main();
