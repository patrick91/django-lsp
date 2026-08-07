import * as vscode from "vscode";
import {
  LanguageClient,
  type LanguageClientOptions,
  type ServerOptions,
} from "vscode-languageclient/node";

import { resolveServer, type ServerSource } from "./server-path";

const CLIENT_ID = "djangoLsp";
const CLIENT_NAME = "Django ORM Language Server";
const INSTALLATION_GUIDE = vscode.Uri.parse(
  "https://github.com/patrick91/django-lsp/tree/main/extensions/vscode-extension#server-resolution",
);

let client: LanguageClient | undefined;
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

async function startClient(
  context: vscode.ExtensionContext,
  reportErrors: boolean,
): Promise<void> {
  try {
    const configuration = vscode.workspace.getConfiguration("djangoLsp");
    const resolution = await resolveServer({
      bundledRoot: context.asAbsolutePath("server"),
      configuredPath: configuration.get<string>("server.path"),
    });
    outputChannel?.appendLine(
      `Starting ${resolution.command} (${sourceLabel(resolution.source)})`,
    );

    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    const serverOptions: ServerOptions = {
      command: resolution.command,
      args: [],
      options: {
        cwd: workspaceFolder?.uri.fsPath,
        env: process.env,
      },
    };
    const clientOptions: LanguageClientOptions = {
      documentSelector: [{ language: "python", scheme: "file" }],
      outputChannel,
      workspaceFolder,
    };

    client = new LanguageClient(
      CLIENT_ID,
      CLIENT_NAME,
      serverOptions,
      clientOptions,
    );
    await client.start();
  } catch (error) {
    client = undefined;
    if (reportErrors) {
      await showStartupError(error);
    } else {
      const message = error instanceof Error ? error.message : String(error);
      outputChannel?.appendLine(`Failed to restart django-lsp: ${message}`);
      throw error;
    }
  }
}

async function stopClient(): Promise<void> {
  const activeClient = client;
  client = undefined;
  if (activeClient) {
    await activeClient.stop();
  }
}

function queueRestart(
  context: vscode.ExtensionContext,
  reportErrors: boolean,
): Promise<void> {
  lifecycle = lifecycle
    .catch(() => undefined)
    .then(async () => {
      await stopClient();
      await startClient(context, reportErrors);
    });
  return lifecycle;
}

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  outputChannel = vscode.window.createOutputChannel(CLIENT_NAME, { log: true });
  context.subscriptions.push(outputChannel);
  context.subscriptions.push(
    vscode.commands.registerCommand("djangoLsp.restartServer", async () => {
      try {
        await queueRestart(context, false);
        await vscode.window.showInformationMessage("django-lsp restarted.");
      } catch (error) {
        await showStartupError(error);
      }
    }),
  );
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (event.affectsConfiguration("djangoLsp.server.path")) {
        void queueRestart(context, true);
      }
    }),
  );

  await startClient(context, true);
}

export async function deactivate(): Promise<void> {
  await stopClient();
  outputChannel = undefined;
}
