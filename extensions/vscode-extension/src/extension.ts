import * as path from "node:path";
import * as vscode from "vscode";
import {
  LanguageClient,
  type LanguageClientOptions,
  type ServerOptions,
} from "vscode-languageclient/node";

import { resolveServer, type ServerSource } from "./server-path";
import { resolveWorkspaceRoot } from "./workspace-root";

const CLIENT_ID = "djangoLsp";
const CLIENT_NAME = "Django ORM Language Server";
const INSTALLATION_GUIDE = vscode.Uri.parse(
  "https://github.com/patrick91/django-lsp/tree/main/extensions/vscode-extension#server-resolution",
);

const clients = new Map<string, LanguageClient>();
let outputChannel: vscode.LogOutputChannel | undefined;
let lifecycle = Promise.resolve();

function sourceLabel(source: ServerSource): string {
  switch (source) {
    case "configured":
      return "configured executable";
    case "path":
      return "PATH executable";
    case "bundled":
      return "bundled executable";
  }
}

async function showStartupError(error: unknown): Promise<void> {
  const message = error instanceof Error ? error.message : String(error);
  outputChannel?.appendLine(`Failed to start django-lsp: ${message}`);
  const action = await vscode.window.showErrorMessage(
    `django-lsp could not start: ${message}`,
    "Open Settings",
    "Installation Guide",
  );
  if (action === "Open Settings") {
    await vscode.commands.executeCommand(
      "workbench.action.openSettings",
      "@ext:patrick91.django-lsp djangoLsp.server.path",
    );
  } else if (action === "Installation Guide") {
    await vscode.env.openExternal(INSTALLATION_GUIDE);
  }
}

function projectFolder(
  root: string,
  workspaceFolder: vscode.WorkspaceFolder,
): vscode.WorkspaceFolder {
  if (root === workspaceFolder.uri.fsPath) {
    return workspaceFolder;
  }
  return {
    index: workspaceFolder.index,
    name: `${workspaceFolder.name}: ${path.basename(root)}`,
    uri: vscode.Uri.file(root),
  };
}

function clientKey(root: string): string {
  const resolved = path.resolve(root);
  return process.platform === "win32" ? resolved.toLowerCase() : resolved;
}

async function startClientForDocument(
  context: vscode.ExtensionContext,
  document: vscode.TextDocument,
): Promise<void> {
  if (document.languageId !== "python" || document.uri.scheme !== "file") {
    return;
  }
  const workspaceFolder = vscode.workspace.getWorkspaceFolder(document.uri);
  if (!workspaceFolder) {
    return;
  }

  const configuration = vscode.workspace.getConfiguration(
    "djangoLsp",
    document.uri,
  );
  const root = await resolveWorkspaceRoot({
    configuredRoot: configuration.get<string>("workspaceRoot"),
    documentPath: document.uri.fsPath,
    workspaceFolderPath: workspaceFolder.uri.fsPath,
  });
  const key = clientKey(root);
  if (clients.has(key)) {
    return;
  }

  const resolution = await resolveServer({
    bundledRoot: context.asAbsolutePath("server"),
    configuredPath: configuration.get<string>("server.path"),
  });
  outputChannel?.appendLine(
    `Starting ${resolution.command} for ${root} (${sourceLabel(resolution.source)})`,
  );

  const workspace = projectFolder(root, workspaceFolder);
  const serverOptions: ServerOptions = {
    command: resolution.command,
    args: [],
    options: {
      cwd: root,
      env: process.env,
    },
  };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [
      {
        language: "python",
        scheme: "file",
        pattern: { baseUri: workspace.uri.toString(), pattern: "**/*.py" },
      },
    ],
    outputChannel,
    workspaceFolder: workspace,
  };

  const nextClient = new LanguageClient(
    CLIENT_ID,
    `${CLIENT_NAME} (${path.basename(root)})`,
    serverOptions,
    clientOptions,
  );
  clients.set(key, nextClient);
  try {
    await nextClient.start();
  } catch (error) {
    clients.delete(key);
    throw error;
  }
}

async function startClientsForOpenDocuments(
  context: vscode.ExtensionContext,
): Promise<void> {
  for (const document of vscode.workspace.textDocuments) {
    await startClientForDocument(context, document);
  }
}

async function stopClients(): Promise<void> {
  const activeClients = [...clients.values()];
  clients.clear();
  await Promise.all(activeClients.map((activeClient) => activeClient.stop()));
}

async function restartClients(context: vscode.ExtensionContext): Promise<void> {
  await stopClients();
  await startClientsForOpenDocuments(context);
}

function queueOperation(operation: () => Promise<void>): Promise<void> {
  lifecycle = lifecycle
    .catch(() => undefined)
    .then(operation);
  return lifecycle;
}

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  outputChannel = vscode.window.createOutputChannel(CLIENT_NAME, { log: true });
  context.subscriptions.push(outputChannel);
  context.subscriptions.push(
    vscode.commands.registerCommand("djangoLsp.restartServer", async () => {
      try {
        await queueOperation(() => restartClients(context));
        await vscode.window.showInformationMessage("django-lsp restarted.");
      } catch (error) {
        await showStartupError(error);
      }
    }),
  );
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (
        event.affectsConfiguration("djangoLsp.server.path") ||
        event.affectsConfiguration("djangoLsp.workspaceRoot")
      ) {
        void queueOperation(() => restartClients(context)).catch(showStartupError);
      }
    }),
  );
  context.subscriptions.push(
    vscode.workspace.onDidOpenTextDocument((document) => {
      void queueOperation(() => startClientForDocument(context, document)).catch(
        showStartupError,
      );
    }),
  );
  context.subscriptions.push(
    vscode.workspace.onDidChangeWorkspaceFolders(() => {
      void queueOperation(() => restartClients(context)).catch(showStartupError);
    }),
  );

  try {
    await queueOperation(() => startClientsForOpenDocuments(context));
  } catch (error) {
    await showStartupError(error);
  }
}

export async function deactivate(): Promise<void> {
  await lifecycle.catch(() => undefined);
  await stopClients();
  outputChannel = undefined;
}
