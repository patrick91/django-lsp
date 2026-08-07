import { constants } from "node:fs";
import { access } from "node:fs/promises";
import * as path from "node:path";

interface FileSystemOperations {
  access(candidate: string, mode: number): Promise<void>;
}

export interface ResolveWorkspaceRootOptions {
  configuredRoot?: string;
  documentPath: string;
  fileSystem?: FileSystemOperations;
  workspaceFolderPath: string;
}

const defaultFileSystem: FileSystemOperations = { access };

async function exists(
  candidate: string,
  fileSystem: FileSystemOperations,
): Promise<boolean> {
  try {
    await fileSystem.access(candidate, constants.F_OK);
    return true;
  } catch {
    return false;
  }
}

function directoriesToWorkspace(
  documentPath: string,
  workspaceFolderPath: string,
): string[] {
  const workspace = path.resolve(workspaceFolderPath);
  let current = path.dirname(path.resolve(documentPath));
  const relative = path.relative(workspace, current);
  if (relative.startsWith("..") || path.isAbsolute(relative)) {
    return [];
  }

  const directories = [];
  while (true) {
    directories.push(current);
    if (current === workspace) {
      break;
    }
    current = path.dirname(current);
  }
  return directories;
}

async function findMarker(
  directories: string[],
  marker: string,
  fileSystem: FileSystemOperations,
): Promise<string | undefined> {
  for (const directory of directories) {
    if (await exists(path.join(directory, marker), fileSystem)) {
      return directory;
    }
  }
  return undefined;
}

export async function resolveWorkspaceRoot(
  options: ResolveWorkspaceRootOptions,
): Promise<string> {
  const fileSystem = options.fileSystem ?? defaultFileSystem;
  const workspaceFolder = path.resolve(options.workspaceFolderPath);
  const configuredRoot = options.configuredRoot?.trim();

  if (configuredRoot) {
    const resolved = path.isAbsolute(configuredRoot)
      ? path.normalize(configuredRoot)
      : path.resolve(workspaceFolder, configuredRoot);
    if (!(await exists(resolved, fileSystem))) {
      throw new Error(`Configured django-lsp workspace root does not exist: ${resolved}`);
    }
    return resolved;
  }

  const directories = directoriesToWorkspace(
    options.documentPath,
    workspaceFolder,
  );
  return (
    (await findMarker(directories, "manage.py", fileSystem)) ??
    (await findMarker(directories, "pyproject.toml", fileSystem)) ??
    workspaceFolder
  );
}
