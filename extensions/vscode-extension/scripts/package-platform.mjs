import { constants, readFileSync } from "node:fs";
import { chmod, copyFile, mkdir, rmdir, unlink } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const extensionRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
);
const [target, binaryArgument, outputArgument = "dist"] = process.argv.slice(2);
const targets = new Map([
  ["darwin-arm64", "django-lsp"],
  ["darwin-x64", "django-lsp"],
  ["linux-arm64", "django-lsp"],
  ["linux-x64", "django-lsp"],
  ["win32-x64", "django-lsp.exe"],
]);

if (!target || !binaryArgument || !targets.has(target)) {
  throw new Error(
    "usage: npm run package:platform -- <darwin-arm64|darwin-x64|linux-arm64|linux-x64|win32-x64> <server-binary> [output-directory]",
  );
}

const packageJson = JSON.parse(
  readFileSync(path.join(extensionRoot, "package.json"), "utf8"),
);
const sourceBinary = path.resolve(process.cwd(), binaryArgument);
const serverDirectory = path.join(extensionRoot, "server");
const stagedBinary = path.join(serverDirectory, targets.get(target));
const outputDirectory = path.resolve(process.cwd(), outputArgument);
const outputPath = path.join(
  outputDirectory,
  `django-lsp-${packageJson.version}-${target}.vsix`,
);

await mkdir(serverDirectory, { recursive: true });
await mkdir(outputDirectory, { recursive: true });

let staged = false;
try {
  await copyFile(sourceBinary, stagedBinary, constants.COPYFILE_EXCL);
  staged = true;
  if (target !== "win32-x64") {
    await chmod(stagedBinary, 0o755);
  }

  const result = spawnSync(
    "npm",
    [
      "exec",
      "--",
      "vsce",
      "package",
      "--no-dependencies",
      "--target",
      target,
      "--out",
      outputPath,
    ],
    {
      cwd: extensionRoot,
      shell: process.platform === "win32",
      stdio: "inherit",
    },
  );
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(`vsce exited with status ${result.status}`);
  }
} finally {
  if (staged) {
    await unlink(stagedBinary).catch(() => undefined);
  }
  await rmdir(serverDirectory).catch(() => undefined);
}
