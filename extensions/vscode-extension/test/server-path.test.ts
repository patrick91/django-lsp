import * as assert from "node:assert/strict";
import { chmod, mkdtemp, mkdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import * as path from "node:path";
import { test } from "node:test";

import { resolveServer } from "../src/server-path";

async function temporaryDirectory(): Promise<string> {
  return mkdtemp(path.join(tmpdir(), "django-lsp-vscode-"));
}

async function executable(filePath: string): Promise<void> {
  await mkdir(path.dirname(filePath), { recursive: true });
  await writeFile(filePath, "test");
  await chmod(filePath, 0o755);
}

test("configured executable takes precedence over PATH", async (context) => {
  const root = await temporaryDirectory();
  context.after(() => rm(root, { recursive: true, force: true }));
  const configured = path.join(root, "configured", "django-lsp");
  const fromPath = path.join(root, "path", "django-lsp");
  await executable(configured);
  await executable(fromPath);

  const resolution = await resolveServer({
    bundledRoot: path.join(root, "bundle"),
    configuredPath: configured,
    env: { PATH: path.dirname(fromPath) },
    platform: "darwin",
  });

  assert.deepEqual(resolution, { command: configured, source: "configured" });
});

test("PATH executable is used before a bundled server", async (context) => {
  const root = await temporaryDirectory();
  context.after(() => rm(root, { recursive: true, force: true }));
  const fromPath = path.join(root, "path", "django-lsp");
  const bundled = path.join(root, "bundle", "django-lsp");
  await executable(fromPath);
  await executable(bundled);

  const resolution = await resolveServer({
    bundledRoot: path.dirname(bundled),
    env: { PATH: path.dirname(fromPath) },
    platform: "linux",
  });

  assert.deepEqual(resolution, { command: fromPath, source: "path" });
});

test("bundled Unix server is made executable", async (context) => {
  const root = await temporaryDirectory();
  context.after(() => rm(root, { recursive: true, force: true }));
  const bundled = path.join(root, "bundle", "django-lsp");
  await mkdir(path.dirname(bundled), { recursive: true });
  await writeFile(bundled, "test", { mode: 0o600 });

  const resolution = await resolveServer({
    bundledRoot: path.dirname(bundled),
    env: { PATH: "" },
    platform: "linux",
  });

  assert.deepEqual(resolution, { command: bundled, source: "bundled" });
  assert.notEqual((await stat(bundled)).mode & 0o111, 0);
});

test("Windows PATH lookup honors PATHEXT", async (context) => {
  const root = await temporaryDirectory();
  context.after(() => rm(root, { recursive: true, force: true }));
  const command = path.join(root, "bin", "django-lsp.exe");
  await executable(command);

  const resolution = await resolveServer({
    bundledRoot: path.join(root, "bundle"),
    env: { PATH: path.dirname(command), PATHEXT: ".EXE;.CMD" },
    platform: "win32",
  });

  assert.deepEqual(resolution, { command, source: "path" });
});

test("missing server produces an actionable error", async (context) => {
  const root = await temporaryDirectory();
  context.after(() => rm(root, { recursive: true, force: true }));

  await assert.rejects(
    resolveServer({
      bundledRoot: path.join(root, "bundle"),
      env: { PATH: "" },
      platform: "darwin",
    }),
    /Configure djangoLsp\.server\.path, install django-lsp on PATH, or install a platform-specific VSIX/,
  );
});
