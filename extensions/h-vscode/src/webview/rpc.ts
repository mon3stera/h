/**
 * Webview-side RPC bridge to the extension host.
 *
 * The extension mirrors the `h serve` wire protocol 1:1 (see src/server.ts),
 * so this side sees the same request/response/notification shapes:
 *   - request → { type: 'request', id, method, params } → reply by id
 *   - response → { type: 'response', id, result | error }
 *   - notification → { type: 'notification', method, params }
 *   - server-initiated request (ask/question) → { type: 'request', id, ... }
 */

interface VsCodeApi {
  postMessage(message: unknown): void;
}

declare function acquireVsCodeApi(): VsCodeApi;

type Pending = {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
};

interface ExtensionMessage {
  type: string;
  id?: number;
  method?: string;
  params?: unknown;
  result?: unknown;
  error?: { code: number; message: string };
}

const vscode = acquireVsCodeApi();

let nextId = 1;
const pending = new Map<number, Pending>();
const notificationHandlers = new Map<string, Set<(params: unknown) => void>>();
const requestHandlers = new Map<string, Set<(params: unknown, id: number) => void>>();

window.addEventListener('message', (event: MessageEvent<ExtensionMessage>) => {
  const message = event.data;

  if (message.type === 'response' && typeof message.id === 'number') {
    const entry = pending.get(message.id);
    if (!entry) return;
    pending.delete(message.id);
    if (message.error) {
      entry.reject(new Error(message.error.message));
    } else {
      entry.resolve(message.result);
    }
    return;
  }

  if (message.type === 'notification' && typeof message.method === 'string') {
    for (const handler of [...(notificationHandlers.get(message.method) ?? [])]) handler(message.params);
    return;
  }

  if (message.type === 'request' && typeof message.method === 'string' && typeof message.id === 'number') {
    for (const handler of [...(requestHandlers.get(message.method) ?? [])]) handler(message.params, message.id);
  }
});

export function request<T = unknown>(method: string, params?: unknown): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const id = nextId++;
    pending.set(id, { resolve: resolve as (value: unknown) => void, reject });
    vscode.postMessage({ type: 'request', id, method, params });
  });
}

export function respond(id: number, result: unknown): void {
  vscode.postMessage({ type: 'respond', id, result });
}

/** Asks the extension host to open a URL in the system browser. */
export function openExternal(url: string): void {
  vscode.postMessage({ type: 'open-external', url });
}

export function onNotification(method: string, handler: (params: unknown) => void): () => void {
  return subscribe(notificationHandlers, method, handler);
}

export function onRequest(method: string, handler: (params: unknown, id: number) => void): () => void {
  return subscribe(requestHandlers, method, handler);
}

function subscribe<K>(registry: Map<string, Set<K>>, method: string, handler: K): () => void {
  let handlers = registry.get(method);
  if (!handlers) {
    handlers = new Set();
    registry.set(method, handlers);
  }
  handlers.add(handler);
  return () => {
    handlers.delete(handler);
    if (handlers.size === 0) registry.delete(method);
  };
}
