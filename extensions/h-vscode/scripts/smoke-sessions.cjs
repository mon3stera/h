/**
 * Smoke test for the M3 session-lifecycle surface against a real `h serve`:
 * session/list → create → turn → close (archives) → resume (replays the
 * transcript) → attach (replays again) → /clear → /compact → close.
 *
 *   cargo build
 *   npm run build:extension
 *   node scripts/smoke-sessions.cjs [path-to-h]
 */
'use strict';

const { HServer } = require('../dist/server.js');

const hPath = process.argv[2] || process.env.H_BIN || 'h';

function waitFor(server, sessionId, predicate, label, timeoutMs = 60_000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      off();
      reject(new Error(`timed out waiting for ${label}`));
    }, timeoutMs);
    const off = server.onNotification('session/event', (params) => {
      if (params.session_id !== sessionId) return;
      if (predicate(params.event)) {
        clearTimeout(timer);
        off();
        resolve(params.event);
      }
    });
  });
}

function collectSince(server, sessionId) {
  const events = [];
  const off = server.onNotification('session/event', (params) => {
    if (params.session_id === sessionId) events.push(params.event);
  });
  return { events, off };
}

async function main() {
  const server = await HServer.start(hPath);
  console.log(`hello: version=${server.hello.version}`);

  let list = await server.request('session/list', {});
  console.log(`initial: ${list.active.length} active, ${list.archived.length} archived`);

  const created = await server.request('session/create', {});
  const id = created.session_id;
  console.log(`created: ${id}`);

  const turn = collectSince(server, id);
  await server.request('turn/submit', { session_id: id, text: 'Reply with the single word: hello' });
  const finished = await waitFor(server, id, (e) => e.type === 'turn_finished' || e.type === 'error', 'turn end');
  turn.off();
  const reply = turn.events.filter((e) => e.type === 'text_delta').map((e) => e.data).join('');
  console.log(`reply: ${JSON.stringify(reply)} (${finished.type})`);
  if (!reply.trim()) throw new Error('turn produced no text');

  await server.request('session/close', { session_id: id });
  list = await server.request('session/list', {});
  console.log(`after close: ${list.active.length} active, ${list.archived.length} archived`);
  if (!list.archived.some((session) => session.id === id)) throw new Error('closed session not archived');

  // Resume replays the transcript: prompt → text_delta → completed.
  const replay = collectSince(server, id);
  await server.request('session/resume', { session_id: id });
  await waitFor(server, id, (e) => e.type === 'completed', 'resume replay completed');
  await new Promise((resolve) => setTimeout(resolve, 300));
  replay.off();
  const replayTypes = replay.events.map((e) => e.type);
  console.log(`resume replay: ${JSON.stringify(replayTypes)}`);
  for (const expected of ['prompt', 'text_delta', 'completed']) {
    if (!replayTypes.includes(expected)) throw new Error(`resume replay missing ${expected}`);
  }

  // Attach to the live session replays again.
  const attach = collectSince(server, id);
  await server.request('session/attach', { session_id: id });
  await waitFor(server, id, (e) => e.type === 'completed', 'attach replay completed');
  await new Promise((resolve) => setTimeout(resolve, 300));
  attach.off();
  const attachTypes = attach.events.map((e) => e.type);
  console.log(`attach replay: ${JSON.stringify(attachTypes)}`);
  if (!attachTypes.includes('prompt') || !attachTypes.includes('text_delta')) {
    throw new Error('attach replay did not rebuild the transcript');
  }

  // /clear emits session_started then command_finished.
  const clearEvents = collectSince(server, id);
  await server.request('command/run', { session_id: id, command: '/clear' });
  const clearEnd = await waitFor(server, id, (e) => e.type === 'command_finished', 'clear command_finished');
  await new Promise((resolve) => setTimeout(resolve, 200));
  clearEvents.off();
  console.log(`/clear: ${JSON.stringify(clearEvents.events.map((e) => e.type))} data=${JSON.stringify(clearEnd.data)}`);
  if (!clearEvents.events.some((e) => e.type === 'session_started')) throw new Error('/clear did not emit session_started');

  // /compact: provider-dependent, but command_finished (or error) must arrive.
  const compactEvents = collectSince(server, id);
  await server.request('command/run', { session_id: id, command: '/compact' });
  await waitFor(server, id, (e) => e.type === 'command_finished' || e.type === 'error', 'compact end');
  await new Promise((resolve) => setTimeout(resolve, 200));
  compactEvents.off();
  console.log(`/compact: ${JSON.stringify(compactEvents.events.map((e) => e.type))}`);

  await server.request('session/close', { session_id: id });
  await server.dispose();
  console.log('smoke-sessions OK');
}

main().catch((error) => {
  console.error(`\nSMOKE FAILED: ${error.message}`);
  process.exitCode = 1;
});
