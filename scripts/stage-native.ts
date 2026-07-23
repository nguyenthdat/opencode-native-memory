import { cp, mkdir, readdir, rm } from "node:fs/promises";
import { basename, join, resolve } from "node:path";

interface Options {
  target?: string;
  profile: "debug" | "release";
  destination: string;
  runtime: boolean;
}

async function main(): Promise<void> {
  const options = parseOptions(process.argv.slice(2));
  const targetPrefix = options.target ? join("target", options.target) : "target";
  const buildRoot = resolve(targetPrefix, options.profile);
  const executable = process.platform === "win32" ? "opencode-memory.exe" : "opencode-memory";
  const sourceBinary = join(buildRoot, executable);
  const destination = resolve(options.destination);
  const binaryDirectory = join(destination, "bin");
  const libraryDirectory = join(binaryDirectory, "memory-libs");

  const libraryName =
    process.platform === "darwin"
      ? "libzvec_c_api.dylib"
      : process.platform === "win32"
        ? "zvec_c_api.dll"
        : "libzvec_c_api.so";
  const library = await findFile(join(buildRoot, "build"), libraryName);
  if (!library) throw new Error(`Cannot find ${libraryName} below ${buildRoot}`);

  if (options.runtime) {
    const runtimeLibraryDirectory =
      process.platform === "win32" ? destination : join(destination, "memory-libs");
    await mkdir(runtimeLibraryDirectory, { recursive: true });
    await cp(library, join(runtimeLibraryDirectory, libraryName));
    return;
  }

  await rm(binaryDirectory, { recursive: true, force: true });
  await mkdir(libraryDirectory, { recursive: true });
  await cp(sourceBinary, join(binaryDirectory, executable));

  const libraryDestination =
    process.platform === "win32"
      ? join(binaryDirectory, libraryName)
      : join(libraryDirectory, libraryName);
  await cp(library, libraryDestination);
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

await main();
