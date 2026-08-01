/**
 * Wire types for the `h serve` JSON-RPC protocol.
 *
 * Mirrors `src/serve/protocol.rs` and the event shapes derived in
 * `crates/h-core/src/event.rs`. Keep in sync with
 * docs/vscode-integration.md §3 — do not let this file drift.
 */

export const PROTOCOL_VERSION = 1;

// Standard JSON-RPC error codes.
export const PARSE_ERROR = -32700;
export const INVALID_REQUEST = -32600;
export const METHOD_NOT_FOUND = -32601;
export const INVALID_PARAMS = -32602;
// Application error codes.
export const SESSION_NOT_FOUND = -32000;
export const SESSION_BUSY = -32001;
export const RESUME_REFUSED = -32002;
export const PROFILE_ERROR = -32003;
export const INIT_ERROR = -32004;

export interface RpcErrorShape {
  code: number;
  message: string;
}

// --- server -> client notifications ---------------------------------------

export interface HelloParams {
  protocol_version: number;
  version: string;
  pid: number;
}

export interface SessionStartedParams {
  session_id: string;
  model: string;
  thinking_effort?: string | null;
}

export interface SessionEventParams {
  session_id: string;
  event: ViewEvent;
}

/**
 * Adjacent-tagged view events: `{type, data}`, one per `AgentViewEvent`
 * variant. See docs/vscode-integration.md §3.4.
 */
export type ViewEvent =
  | { type: 'prompt'; data: string }
  | { type: 'text_delta'; data: string }
  | { type: 'search'; data: SearchView }
  | { type: 'tool'; data: ToolPresentation }
  | { type: 'turn_start'; data: null }
  | { type: 'token_usage'; data: TokenUsage }
  | { type: 'session_started'; data: null }
  | { type: 'command_finished'; data: '/clear' | '/compact' }
  | { type: 'context_compacted'; data: null }
  | { type: 'turn_finished'; data: { completed: boolean } }
  | { type: 'completed'; data: null }
  | { type: 'error'; data: string };

export interface SearchView {
  id: string;
  status: string;
  action: unknown;
}

export interface TokenUsage {
  context?: number;
  turn?: number;
}

// --- server -> client requests (ask/question) ------------------------------

export interface AskQuestionParams {
  session_id: string;
  question: string;
  options: AskOption[];
}

export interface AskOption {
  label: string;
  description?: string;
}

export type AskAnswer =
  | { type: 'option'; data: { index: number; label: string } }
  | { type: 'free_text'; data: string };

// --- client -> server requests (results) -----------------------------------

export interface SessionCreated {
  session_id: string;
}

export interface Accepted {
  accepted: boolean;
}

export interface Replayed {
  replayed: boolean;
}

export interface SessionList {
  archived: ArchivedSession[];
  active: { id: string }[];
}

export interface ArchivedSession {
  id: string;
  title: string;
  last_modified: string;
}

// --- tool presentation (the `tool` event payload) --------------------------
// Shapes mirror `tool/presentation.rs`. The webview renders these at M3; the
// types are pinned now so the wire contract cannot drift silently.

export interface ToolPresentation {
  call_id: string;
  name: string;
  label: string;
  target?: string;
  status: ToolCallStatus;
  blocks: DisplayBlock[];
}

export type ToolCallStatus =
  | { type: 'running'; data: null }
  | { type: 'succeeded'; data: null }
  | { type: 'failed'; data: { message: string } };

export type DisplayBlock =
  | { type: 'summary'; data: string }
  | {
      type: 'code_block';
      data: {
        language?: string | null;
        content: string;
        truncated_lines: number;
        show_line_numbers: boolean;
        start_line_number: number;
      };
    }
  | { type: 'diff'; data: { lines: DiffLine[] } }
  | { type: 'table'; data: { headers: string[]; rows: string[][] } }
  | { type: 'key_value'; data: { entries: KeyValueEntry[] } }
  | { type: 'text_output'; data: { content: string; truncated_lines: number } };

export interface DiffLine {
  number: number;
  kind: 'removed' | 'added' | 'context';
  text: string;
}

export interface KeyValueEntry {
  key: string;
  value: string;
}
