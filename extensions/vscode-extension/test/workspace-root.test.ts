import * as assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import * as path from "node:path";
import { test } from "node:test";

import { resolveWorkspaceRoot } from "../src/workspace-root";

async function temporaryDirectory(): Promise<string> {
  return mkdtemp(path.join(tmpdir(), "django-lsp-workspace-"));
}

async function touch(filePath: string): Promise<void> {
  await mkdir(path.dirname(filePath), { recursive: true });
  await writeFile(filePath, "");
}

test("configured relative root takes precedence", async (context) => {
  const workspace = await temporaryDirectory();
  context.after(() => rm(workspace, { recursive: true, force: true }));
  const document = path.join(workspace, "services", "api", "views.py");
  await touch(document);
  await mkdir(path.join(workspace, "backend"));
  await touch(path.join(workspace, "services", "manage.py"));

  const root = await resolveWorkspaceRoot({
    configuredRoot: "backend",
    documentPath: document,
    workspaceFolderPath: workspace,
  });

  assert.equal(root, path.join(workspace, "backend"));
});

test("nearest manage.py defines a Django project in a monorepo", async (context) => {
  const workspace = await temporaryDirectory();
  context.after(() => rm(workspace, { recursive: true, force: true }));
  const document = path.join(workspace, "backend", "api", "views.py");
  await touch(document);
  await touch(path.join(workspace, "backend", "manage.py"));
  await touch(path.join(workspace, "pyproject.toml"));

  const root = await resolveWorkspaceRoot({
    documentPath: document,
    workspaceFolderPath: workspace,
  });

  assert.equal(root, path.join(workspace, "backend"));
});

test("nearest pyproject.toml is used without manage.py", async (context) => {
  const workspace = await temporaryDirectory();
  context.after(() => rm(workspace, { recursive: true, force: true }));
  const document = path.join(workspace, "packages", "api", "views.py");
  await touch(document);
  await touch(path.join(workspace, "packages", "pyproject.toml"));

  const root = await resolveWorkspaceRoot({
    documentPath: document,
    workspaceFolderPath: workspace,
  });

  assert.equal(root, path.join(workspace, "packages"));
});

test("marker search stays inside the containing workspace", async (context) => {
  const parent = await temporaryDirectory();
  context.after(() => rm(parent, { recursive: true, force: true }));
  const workspace = path.join(parent, "workspace");
  const document = path.join(workspace, "api", "views.py");
  await touch(document);
  await touch(path.join(parent, "manage.py"));

  const root = await resolveWorkspaceRoot({
    documentPath: document,
    workspaceFolderPath: workspace,
  });

  assert.equal(root, workspace);
});

test("missing configured root produces an actionable error", async (context) => {
  const workspace = await temporaryDirectory();
  context.after(() => rm(workspace, { recursive: true, force: true }));
  const document = path.join(workspace, "api", "views.py");
  await touch(document);

  await assert.rejects(
    resolveWorkspaceRoot({
      configuredRoot: "missing",
      documentPath: document,
      workspaceFolderPath: workspace,
    }),
    /Configured django-lsp workspace root does not exist/,
  );
});
