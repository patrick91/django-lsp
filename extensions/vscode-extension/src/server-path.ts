import { constants } from "node:fs";
import { access, chmod } from "node:fs/promises";
import * as path from "node:path";

export type ServerSource = "bundled" | "configured" | "path";

export interface ServerResolution {
  command: string;
  source: ServerSource;
}

interface FileSystemOperations {
  access(candidate: string, mode: number): Promise<void>;
  chmod(candidate: string, mode: number): Promise<void>;
}

export interface ResolveServerOptions {
  bundledRoot: string;
  configuredPath?: string;
  env?: NodeJS.ProcessEnv;
  fileSystem?: FileSystemOperations;
  platform?: NodeJS.Platform;
}

const defaultFileSystem: FileSystemOperations = { access, chmod };

function executableMode(platform: NodeJS.Platform): number {
  return platform === "win32" ? constants.F_OK : constants.X_OK;
}

async function isExecutable(
  candidate: string,
  platform: NodeJS.Platform,
  fileSystem: FileSystemOperations,
): Promise<boolean> {
  try {
    await fileSystem.access(candidate, executableMode(platform));
    return true;
  } catch {
    return false;
  }
}

function windowsExecutableNames(env: NodeJS.ProcessEnv): string[] {
  const extensions = (env.PATHEXT ?? ".COM;.EXE;.BAT;.CMD")
    .split(";")
    .filter(Boolean)
    .map((extension) => extension.toLowerCase());
  return extensions.map((extension) => `django-lsp${extension}`);
}

export async function findOnPath(
  env: NodeJS.ProcessEnv,
  platform: NodeJS.Platform,
  fileSystem: FileSystemOperations = defaultFileSystem,
): Promise<string | undefined> {
  const pathValue = env.PATH ?? env.Path ?? env.path;
  if (!pathValue) {
    return undefined;
  }

  const delimiter = platform === "win32" ? ";" : ":";
  const executableNames =
    platform === "win32" ? windowsExecutableNames(env) : ["django-lsp"];

  for (const directory of pathValue.split(delimiter).filter(Boolean)) {
    for (const executableName of executableNames) {
      const candidate = path.join(directory, executableName);
      if (await isExecutable(candidate, platform, fileSystem)) {
        return candidate;
      }
    }
  }

  return undefined;
}

export function bundledExecutableName(platform: NodeJS.Platform): string {
  return platform === "win32" ? "django-lsp.exe" : "django-lsp";
}

export async function resolveServer(
  options: ResolveServerOptions,
): Promise<ServerResolution> {
  const platform = options.platform ?? process.platform;
  const env = options.env ?? process.env;
  const fileSystem = options.fileSystem ?? defaultFileSystem;
  const configuredPath = options.configuredPath?.trim();

  if (configuredPath) {
    if (!(await isExecutable(configuredPath, platform, fileSystem))) {
      throw new Error(
        `Configured django-lsp executable is not runnable: ${configuredPath}`,
      );
    }
    return { command: configuredPath, source: "configured" };
  }

  const pathCommand = await findOnPath(env, platform, fileSystem);
  if (pathCommand) {
    return { command: pathCommand, source: "path" };
  }

  const bundledCommand = path.join(
    options.bundledRoot,
    bundledExecutableName(platform),
  );
  try {
    await fileSystem.access(bundledCommand, constants.F_OK);
    if (platform !== "win32") {
      await fileSystem.chmod(bundledCommand, 0o755);
    }
    if (await isExecutable(bundledCommand, platform, fileSystem)) {
      return { command: bundledCommand, source: "bundled" };
    }
  } catch {
    // Fall through to the actionable installation error.
  }

  throw new Error(
    "No django-lsp executable was found. Configure djangoLsp.server.path, install django-lsp on PATH, or install a platform-specific VSIX.",
  );
}
