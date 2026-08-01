/**
 * A running `h serve` process with a JSON-RPC 2.0 client over stdio.
 *
 * Frame discipline: one JSON object per line, UTF-8, on stdout only. This
 * module is pure Node — it never imports
 * `vscode` — so it can be exercised standalone (see scripts/smoke.cjs).
 */
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { createInterface } from 'node:readline';
import * as protocol from './protocol';

/** Error carrying the JSON-RPC error code the server replied with. */
export class RpcError extends Error {
  constructor(
    readonly code: number,
    message: string,
  ) {
    super(message);
    this.name = 'RpcError';
  }
}

interface PendingRequest {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

type Params = unknown;
type NotificationHandler = (params: Params) => void;
type ServerRequestHandler = (params: Params, id: number) => void;
type ExitHandler = (code: number | null, signal: NodeJS.Signals | null) => void;

interface Incoming {
  id?: unknown;
  method?: unknown;
  params?: Params;
  result?: unknown;
  error?: protocol.RpcErrorShape;
}

const DEFAULT_REQUEST_TIMEOUT_MS = 60_000;
const HELLO_TIMEOUT_MS = 10_000;
const SHUTDOWN_TIMEOUT_MS = 5_000;

export class HServer {
  private readonly proc: ChildProcessWithoutNullStreams;
  private nextId = 1;
  private readonly pending = new Map<number, PendingRequest>();
  private readonly notifications = new Map<string, Set<NotificationHandler>>();
  private readonly requestHandlers = new Map<string, Set<ServerRequestHandler>>();
  private readonly exitHandlers = new Set<ExitHandler>();
  private readonly diagnosticsTail: string[] = [];

  hello?: protocol.HelloParams;

  private constructor(proc: ChildProcessWithoutNullStreams) {
    this.proc = proc;

    const lines = createInterface({ input: proc.stdout, crlfDelay: Infinity });
    lines.on('line', (line) => this.dispatch(line));

    proc.stderr.on('data', (chunk: Buffer) => this.noteDiagnostic(chunk.toString()));
    proc.on('exit', (code, signal) => {
      this.failAllPending(code, signal);
      for (const handler of [...this.exitHandlers]) handler(code, signal);
    });
  }

  /** Spawns `h serve` and waits for the `server/hello` handshake. */
  static async start(hPath: string, timeoutMs: number = HELLO_TIMEOUT_MS): Promise<HServer> {
    const proc = spawn(hPath, ['serve'], { stdio: ['pipe', 'pipe', 'pipe'] });
    const server = new HServer(proc);
    try {
      const hello = await server.waitForHello(timeoutMs);
      if (hello.protocol_version !== protocol.PROTOCOL_VERSION) {
        throw new Error(
          `h speaks protocol ${hello.protocol_version}, this extension expects ` +
            `${protocol.PROTOCOL_VERSION}; rebuild h and reload the window`,
        );
      }
      server.hello = hello;
      return server;
    } catch (error) {
      server.disposeSync();
      throw error;
    }
  }

  /**
   * Sends a request and resolves with the reply. Rejects with [`RpcError`]
   * when the server reports an error, or a plain `Error` on timeout.
   */
  request<T = unknown>(method: string, params?: Params, timeoutMs = DEFAULT_REQUEST_TIMEOUT_MS): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      const id = this.nextId++;
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`request ${method} (id ${id}) timed out after ${timeoutMs}ms`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: resolve as (value: unknown) => void,
        reject,
        timer,
      });
      this.proc.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`, (error) => {
        if (!error) return;
        this.pending.delete(id);
        clearTimeout(timer);
        reject(new Error(`failed to write request ${method}: ${error.message}`));
      });
    });
  }

  /** Replies to a server-initiated request (ask/question → ask/answer). */
  respond(id: number, result: unknown): void {
    this.proc.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, result })}\n`);
  }

  /** Subscribes to server notifications; returns the unsubscribe function. */
  onNotification(method: string, handler: NotificationHandler): () => void {
    return this.subscribe(this.notifications, method, handler);
  }

  /** Subscribes to server-initiated requests (ask/question). */
  onRequest(method: string, handler: ServerRequestHandler): () => void {
    return this.subscribe(this.requestHandlers, method, handler);
  }

  onExit(handler: ExitHandler): () => void {
    this.exitHandlers.add(handler);
    return () => this.exitHandlers.delete(handler);
  }

  /** Graceful shutdown: `server/shutdown`, then SIGTERM as a fallback. */
  async dispose(timeoutMs: number = SHUTDOWN_TIMEOUT_MS): Promise<void> {
    if (this.proc.exitCode !== null || this.proc.signalCode !== null) return;

    const exited = new Promise<void>((resolve) => {
      const handler: ExitHandler = () => {
        this.exitHandlers.delete(handler);
        resolve();
      };
      this.exitHandlers.add(handler);
    });

    try {
      this.proc.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: this.nextId++, method: 'server/shutdown', params: {} })}\n`);
      await Promise.race([exited, new Promise((resolve) => setTimeout(resolve, timeoutMs))]);
    } finally {
      if (this.proc.exitCode === null && this.proc.signalCode === null) {
        this.proc.kill('SIGTERM');
      }
    }
  }

  /** Recent stderr and framing diagnostics, for surfacing startup failures. */
  diagnostics(): string {
    return this.diagnosticsTail.join('\n').trim();
  }

  private subscribe<T extends (params: Params, id: number) => void>(
    registry: Map<string, Set<T>>,
    method: string,
    handler: T,
  ): () => void {
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

  private waitForHello(timeoutMs: number): Promise<protocol.HelloParams> {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        off();
        reject(new Error(`h serve did not send server/hello within ${timeoutMs}ms`));
      }, timeoutMs);
      const off = this.onNotification('server/hello', (params) => {
        clearTimeout(timer);
        resolve(params as protocol.HelloParams);
      });
    });
  }

  private dispatch(line: string): void {
    let message: Incoming;
    try {
      message = JSON.parse(line) as Incoming;
    } catch {
      this.noteDiagnostic(`unparseable frame from h serve: ${line}`);
      return;
    }

    const { id, method } = message;

    // Server-initiated request (ask/question): has both id and method.
    if (typeof id === 'number' && typeof method === 'string') {
      for (const handler of [...(this.requestHandlers.get(method) ?? [])]) handler(message.params, id);
      return;
    }

    // Notification: has a method, no id.
    if (typeof method === 'string') {
      for (const handler of [...(this.notifications.get(method) ?? [])]) handler(message.params);
      return;
    }

    // Response to one of our requests: has an id, no method.
    if (typeof id === 'number') {
      const pending = this.pending.get(id);
      if (!pending) {
        this.noteDiagnostic(`response for unknown id ${id}: ${line}`);
        return;
      }
      this.pending.delete(id);
      clearTimeout(pending.timer);
      if (message.error !== undefined) {
        pending.reject(new RpcError(message.error.code, message.error.message));
      } else {
        pending.resolve(message.result);
      }
      return;
    }

    this.noteDiagnostic(`unclassifiable frame from h serve: ${line}`);
  }

  private noteDiagnostic(text: string): void {
    this.diagnosticsTail.push(text.trim());
    if (this.diagnosticsTail.length > 20) this.diagnosticsTail.shift();
  }

  private failAllPending(code: number | null, signal: NodeJS.Signals | null): void {
    const reason = new Error(`h serve exited unexpectedly (code ${code ?? 'null'}, signal ${signal ?? 'none'})`);
    for (const [id, pending] of this.pending) {
      clearTimeout(pending.timer);
      pending.reject(reason);
      this.pending.delete(id);
    }
  }

  private disposeSync(): void {
    if (this.proc.exitCode === null && this.proc.signalCode === null) {
      this.proc.kill('SIGKILL');
    }
  }
}
