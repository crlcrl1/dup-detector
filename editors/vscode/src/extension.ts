import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

const documentSelector = [
  { scheme: "file", language: "rust" },
  { scheme: "file", language: "python" },
  { scheme: "file", language: "javascript" },
  { scheme: "file", language: "javascriptreact" },
  { scheme: "file", language: "typescript" },
  { scheme: "file", language: "typescriptreact" },
  { scheme: "file", language: "c" },
  { scheme: "file", language: "cpp" },
];

export function activate(context: vscode.ExtensionContext): void {
  const command = vscode.workspace
    .getConfiguration("dup-detector")
    .get<string>("serverPath", "dup-detector");

  const serverOptions: ServerOptions = {
    command,
    args: ["lsp"],
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector,
  };

  client = new LanguageClient(
    "dup-detector",
    "dup-detector",
    serverOptions,
    clientOptions,
  );

  client.start().catch((error: unknown) => {
    void vscode.window.showErrorMessage(
      `dup-detector: failed to start the language server (${String(error)}). ` +
        "Check the `dup-detector.serverPath` setting.",
    );
  });

  context.subscriptions.push({
    dispose: () => {
      void client?.stop();
    },
  });
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}
