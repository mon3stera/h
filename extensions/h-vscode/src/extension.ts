/**
 * Extension host: owns one `h serve` child process shared by every chat panel.
 *
 * Lifecycle per docs/vscode-integration.md §5.2: closing a panel does not
 * close sessions; only VS Code exiting (or an explicit shutdown) tears the
 * server down, at which point stdin EOF makes `h serve` archive every session.
 */
import { execFile } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { promisify } from 'node:util';
import * as vscode from 'vscode';
import * as protocol from './protocol';
import { HServer, RpcError } from './server';

const execFileAsync = promisify(execFile);

/** One server per extension host, reused across panels. */
let server: HServer | null = null;

/** Open chat panels, so a font-size setting change can reach all of them. */
const panels = new Set<vscode.WebviewPanel>();

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  context.subscriptions.push(vscode.commands.registerCommand('h.openChat', () => openChat(context)));
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (!event.affectsConfiguration('h.fontSize')) return;
      const fontSize = readFontSize();
      for (const panel of panels) post(panel, { type: 'font-size', fontSize });
    }),
  );
}

export function deactivate(): void {
  const current = server;
  server = null;
  void current?.dispose();
}

async function openChat(context: vscode.ExtensionContext): Promise<void> {
  if (!server) {
    const hPath = await resolveHPath();
    if (!hPath) {
      void vscode.window.showErrorMessage(
        'h binary not found. Install h and put it on PATH, or set "h.path" to its location.',
      );
      return;
    }
    try {
      server = await HServer.start(hPath);
    } catch (error) {
      server = null;
      void vscode.window.showErrorMessage(`Could not start h serve: ${describe(error)}`);
      return;
    }
    server.onExit(() => {
      // A normal dispose nulls `server` first; only unexpected exits reach here.
      if (server) {
        server = null;
        void vscode.window.showWarningMessage('h serve exited unexpectedly. Run "h: Open Chat" again to restart it.');
      }
    });
  }

  const webviewRoot = vscode.Uri.joinPath(context.extensionUri, 'dist', 'webview');
  const panel = vscode.window.createWebviewPanel(
    'h.chat',
    'h',
    vscode.ViewColumn.Beside,
    {
      enableScripts: true,
      localResourceRoots: [webviewRoot],
    },
  );

  panel.webview.html = await webviewHtml(panel.webview, webviewRoot, readFontSize());

  panels.add(panel);

  const unsubscribe: Array<() => void> = [
    server.onNotification('session/event', (params) =>
      post(panel, { type: 'notification', method: 'session/event', params }),
    ),
    server.onNotification('session/started', (params) =>
      post(panel, { type: 'notification', method: 'session/started', params }),
    ),
    server.onRequest('ask/question', (params, id) =>
      post(panel, { type: 'request', id, method: 'ask/question', params }),
    ),
  ];
  panel.onDidDispose(() => {
    panels.delete(panel);
    for (const off of unsubscribe) off();
  }, null, context.subscriptions);

  panel.webview.onDidReceiveMessage(
    (message: WebviewMessage) => void handleWebviewMessage(panel, message),
    undefined,
    context.subscriptions,
  );
}

interface WebviewMessage {
  type: string;
  id?: number;
  method?: string;
  params?: unknown;
  result?: unknown;
  url?: string;
}

async function handleWebviewMessage(panel: vscode.WebviewPanel, message: WebviewMessage): Promise<void> {
  if (message.type === 'open-external' && typeof message.url === 'string') {
    try {
      void vscode.env.openExternal(vscode.Uri.parse(message.url, true));
    } catch {
      // Ignore malformed URLs from the webview.
    }
    return;
  }

  if (!server) return;

  if (message.type === 'request' && typeof message.id === 'number' && typeof message.method === 'string') {
    try {
      const result = await server.request(message.method, message.params);
      post(panel, { type: 'response', id: message.id, result });
    } catch (error) {
      post(panel, {
        type: 'response',
        id: message.id,
        error: error instanceof RpcError
          ? { code: error.code, message: error.message }
          : { code: protocol.INIT_ERROR, message: describe(error) },
      });
    }
    return;
  }

  if (message.type === 'respond' && typeof message.id === 'number') {
    server.respond(message.id, message.result);
  }
}

function post(panel: vscode.WebviewPanel, message: unknown): void {
  void panel.webview.postMessage(message);
}

async function webviewHtml(
  webview: vscode.Webview,
  root: vscode.Uri,
  fontSize: number | null,
): Promise<string> {
  let html = await readFile(vscode.Uri.joinPath(root, 'index.html').fsPath, 'utf8');

  // Relative asset URLs in the built bundle resolve against this base, which
  // maps to the webview root on disk (vite builds with base: './').
  html = html.replace('<head>', `<head>\n<base href="${webview.asWebviewUri(root).toString()}/">`);

  // `h.fontSize` overrides the inherited editor font size; without it the
  // stylesheet falls back to `--vscode-font-size`.
  if (fontSize !== null) {
    html = html.replace('<head>', `<head>\n<style>:root { --h-font-size: ${fontSize}px; }</style>`);
  }

  const csp = [
    "default-src 'none'",
    `script-src ${webview.cspSource}`,
    `style-src ${webview.cspSource} 'unsafe-inline'`,
    `img-src ${webview.cspSource} data:`,
    'font-src data:',
  ].join('; ');
  html = html.replace('</head>', `<meta http-equiv="Content-Security-Policy" content="${csp}">\n</head>`);
  return html;
}

/** `h.fontSize` in px, or null to follow the editor font size. */
function readFontSize(): number | null {
  const size = vscode.workspace.getConfiguration('h').get<number | null>('fontSize', null);
  return typeof size === 'number' && size > 0 ? size : null;
}

async function resolveHPath(): Promise<string | undefined> {
  const configured = vscode.workspace.getConfiguration('h').get<string>('path', '');
  if (configured.trim()) return configured.trim();

  const command = process.platform === 'win32' ? 'where' : 'which';
  try {
    const { stdout } = await execFileAsync(command, ['h']);
    const first = stdout.split(/\r?\n/).find((line) => line.trim().length > 0);
    return first?.trim();
  } catch {
    return undefined;
  }
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
