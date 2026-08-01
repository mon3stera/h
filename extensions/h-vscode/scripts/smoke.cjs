/**
 * Standalone smoke test for the JSON-RPC client (src/server.ts) against a real
 * `h serve` process — no VS Code needed.
 *
 *   cargo build            # build target/debug/h first
 *   npm run build:extension
 *   node scripts/smoke.cjs [path-to-h]
 *
 * Exercises: hello handshake → session/create → turn/submit with streaming
 * text_delta → turn_finished → session/close (archives) → graceful shutdown.
 */
'use strict';

const { HServer, RpcError } = require('../dist/server.js');

const hPath = process.argv[2] || process.env.H_BIN || 'h';
const TURN_TIMEOUT_MS = 120_000;

/** The live server, for diagnostics when a step fails. */
let activeServer = null;

function waitForEvent(server, sessionId, eventTypes, timeoutMs) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      off();
      reject(new Error(`timed out waiting for ${eventTypes.join('|')} after ${timeoutMs}ms`));
    }, timeoutMs);
    const off = server.onNotification('session/event', (params) => {
      if (params.session_id !== sessionId) return;
      if (eventTypes.includes(params.event.type)) {
        clearTimeout(timer);
        off();
        resolve(params.event);
      }
    });
  });
}

function fail(message, server) {
  const diagnostics = server ? server.diagnostics() : '';
  console.error(`\nSMOKE FAILED: ${message}`);
  if (diagnostics) console.error(`--- h serve diagnostics ---\n${diagnostics}`);
  process.exitCode = 1;
}

async function main() {
  const server = await HServer.start(hPath);
  activeServer = server;
  console.log(`hello: protocol=${server.hello.protocol_version} version=${server.hello.version} pid=${server.hello.pid}`);

  const created = await server.request('session/create', {});
  const sessionId = created.session_id;
  console.log(`session/create: ${sessionId}`);

  let reply = '';
  server.onNotification('session/event', (params) => {
    if (params.session_id !== sessionId) return;
    if (params.event.type === 'text_delta') {
      process.stdout.write(params.event.data);
      reply += params.event.data;
    }
  });

  const accepted = await server.request('turn/submit', { session_id: sessionId, text: 'Reply with the single word: hello' });
  console.log(`turn/submit accepted: ${accepted.accepted}`);

  const finished = await waitForEvent(server, sessionId, ['turn_finished', 'error'], TURN_TIMEOUT_MS);
  console.log(`\nturn finished: type=${finished.type} completed=${finished.completed ?? '-'} reply=${JSON.stringify(reply)}`);
  if (finished.type === 'error') throw new Error(`turn errored: ${finished.data}`);
  if (!reply.trim()) throw new Error('turn produced no text');

  const closed = await server.request('session/close', { session_id: sessionId });
  console.log(`session/close archived: ${closed.archived}`);

  await server.dispose();
  console.log('smoke OK');
}

main().catch((error) => {
  fail(error instanceof RpcError ? `RPC error ${error.code}: ${error.message}` : error.message, activeServer);
});
