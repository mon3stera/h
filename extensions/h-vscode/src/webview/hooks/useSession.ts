import { useCallback, useEffect, useRef, useState } from 'react';
import type {
  AskAnswer,
  AskQuestionParams,
  ArchivedSession,
  SessionEventParams,
  SessionStartedParams,
  TokenUsage,
  ToolPresentation,
  ViewEvent,
  WireImage,
} from '../../protocol';
import { onNotification, onRequest, request, respond } from '../rpc';

export type ContentBlock = { kind: 'text'; text: string } | { kind: 'tool'; tool: ToolPresentation };

export interface ChatMessage {
  id: number;
  role: 'user' | 'assistant' | 'system';
  content: ContentBlock[];
}

export interface PendingQuestion {
  id: number;
  question: string;
  options: { label: string; description?: string }[];
}

export interface Session {
  phase: 'loading' | 'idle' | 'chat';
  sessionId: string | null;
  messages: ChatMessage[];
  busy: boolean;
  error: string | null;
  tokenUsage: TokenUsage | null;
  contextWindow: number | null;
  pendingQuestion: PendingQuestion | null;
  archived: ArchivedSession[];
  active: { id: string }[];
  create: () => void;
  resume: (id: string) => void;
  attach: (id: string) => void;
  closeSession: (id: string) => void;
  refreshList: () => void;
  send: (text: string, images?: WireImage[]) => void;
  cancel: () => void;
  answer: (answer: AskAnswer) => void;
  dismissQuestion: () => void;
}

const SLASH_COMMANDS = new Set(['/clear', '/compact']);

/**
 * Drives the current chat session plus the picker's session list.
 *
 * One session at a time on screen; closing a panel never closes the session
 * (the server owns it). `attach` re-enters an active session whose transcript
 * the server re-broadcasts, which rebuilds the message list from `prompt` and
 * `text_delta` events.
 */
export function useSession(): Session {
  const [phase, setPhase] = useState<'loading' | 'idle' | 'chat'>('loading');
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [tokenUsage, setTokenUsage] = useState<TokenUsage | null>(null);
  const [contextWindow, setContextWindow] = useState<number | null>(null);
  const [pendingQuestion, setPendingQuestion] = useState<PendingQuestion | null>(null);
  const [archived, setArchived] = useState<ArchivedSession[]>([]);
  const [active, setActive] = useState<{ id: string }[]>([]);

  const nextMessageId = useRef(1);
  const turnActive = useRef(false);
  const sessionIdRef = useRef<string | null>(null);

  const setCurrentSession = (id: string | null) => {
    sessionIdRef.current = id;
    setSessionId(id);
  };

  const append = useCallback((role: 'user' | 'system', text: string) => {
    setMessages((previous) => [
      ...previous,
      { id: nextMessageId.current++, role, content: [{ kind: 'text', text }] },
    ]);
  }, []);

  const appendText = useCallback((text: string) => {
    setMessages((previous) => {
      const last = previous[previous.length - 1];
      if (turnActive.current && last && last.role === 'assistant') {
        const content = last.content;
        const tail = content[content.length - 1];
        if (tail?.kind === 'text') {
          return [
            ...previous.slice(0, -1),
            { ...last, content: [...content.slice(0, -1), { kind: 'text', text: tail.text + text }] },
          ];
        }
        return [...previous.slice(0, -1), { ...last, content: [...content, { kind: 'text', text }] }];
      }
      turnActive.current = true;
      return [...previous, { id: nextMessageId.current++, role: 'assistant', content: [{ kind: 'text', text }] }];
    });
  }, []);

  const appendTool = useCallback((tool: ToolPresentation) => {
    setMessages((previous) => {
      const last = previous[previous.length - 1];
      if (last && last.role === 'assistant') {
        return [...previous.slice(0, -1), { ...last, content: [...last.content, { kind: 'tool', tool }] }];
      }
      turnActive.current = true;
      return [...previous, { id: nextMessageId.current++, role: 'assistant', content: [{ kind: 'tool', tool }] }];
    });
  }, []);

  const handleEvent = useCallback(
    (event: ViewEvent) => {
      switch (event.type) {
        case 'text_delta':
          appendText(event.data);
          break;
        case 'tool':
          appendTool(event.data);
          break;
        case 'prompt':
          append('user', event.data);
          break;
        case 'session_started':
          setMessages([]);
          break;
        case 'context_compacted':
          append('system', 'Context compacted');
          break;
        case 'command_finished':
          append('system', event.data);
          break;
        case 'token_usage':
          setTokenUsage(event.data);
          break;
        case 'completed':
          turnActive.current = false;
          break;
        case 'turn_finished':
          turnActive.current = false;
          setBusy(false);
          break;
        case 'error':
          turnActive.current = false;
          setBusy(false);
          setError(event.data);
          break;
        default:
          break;
      }
    },
    [append, appendText, appendTool],
  );

  useEffect(() => {
    return onNotification('session/event', (params) => {
      const { session_id, event } = params as SessionEventParams;
      if (session_id !== sessionIdRef.current) return;
      handleEvent(event);
    });
  }, [handleEvent]);

  useEffect(() => {
    return onNotification('session/started', (params) => {
      const { session_id, context_window } = params as SessionStartedParams;
      if (session_id !== sessionIdRef.current) return;
      setContextWindow(context_window ?? null);
    });
  }, []);

  useEffect(() => {
    return onRequest('ask/question', (params, id) => {
      const ask = params as AskQuestionParams;
      if (ask.session_id !== sessionIdRef.current) return;
      setPendingQuestion({ id, question: ask.question, options: ask.options });
    });
  }, []);

  const refreshList = useCallback(() => {
    void request<{ archived: ArchivedSession[]; active: { id: string }[] }>('session/list', {})
      .then((list) => {
        setArchived(list.archived);
        setActive(list.active);
      })
      .catch((cause) => setError(describe(cause)));
  }, []);

  useEffect(() => {
    refreshList();
    setPhase('idle');
  }, [refreshList]);

  const create = useCallback(() => {
    setPhase('loading');
    setMessages([]);
    setError(null);
    setPendingQuestion(null);
    void request<{ session_id: string; context_window?: number }>('session/create', {})
      .then((result) => {
        setCurrentSession(result.session_id);
        setContextWindow(result.context_window ?? null);
        setPhase('chat');
      })
      .catch((cause) => {
        setPhase('idle');
        setError(describe(cause));
      });
  }, []);

  const resume = useCallback((id: string) => {
    setPhase('loading');
    setMessages([]);
    setError(null);
    setPendingQuestion(null);
    void request<{ session_id: string; context_window?: number }>('session/resume', {
      session_id: id,
    })
      .then((result) => {
        setCurrentSession(result.session_id);
        setContextWindow(result.context_window ?? null);
        setPhase('chat');
      })
      .catch((cause) => {
        setPhase('idle');
        setError(describe(cause));
      });
  }, []);

  const attach = useCallback((id: string) => {
    setPhase('loading');
    setMessages([]);
    setError(null);
    setPendingQuestion(null);
    void request<{ replayed: boolean; context_window?: number }>('session/attach', {
      session_id: id,
    })
      .then((result) => {
        setCurrentSession(id);
        setContextWindow(result.context_window ?? null);
        setPhase('chat');
      })
      .catch((cause) => {
        setPhase('chat');
        setError(describe(cause));
      });
  }, []);

  const closeSession = useCallback(
    (id: string) => {
      setPhase('loading');
      void request<{ archived: boolean }>('session/close', { session_id: id })
        .then(() => {
          if (sessionIdRef.current === id) {
            setCurrentSession(null);
            setMessages([]);
            setTokenUsage(null);
            setContextWindow(null);
            setPendingQuestion(null);
            setPhase('idle');
          }
          refreshList();
        })
        .catch((cause) => {
          setPhase('chat');
          setError(describe(cause));
        });
    },
    [refreshList],
  );

  const send = useCallback(
    (text: string, images: WireImage[] = []) => {
      const trimmed = text.trim();
      const id = sessionIdRef.current;
      if (!id || (!trimmed && images.length === 0) || busy) return;

      if (SLASH_COMMANDS.has(trimmed)) {
        void request('command/run', { session_id: id, command: trimmed }).catch((cause) =>
          setError(describe(cause)),
        );
        return;
      }

      append('user', trimmed || '(image)');
      turnActive.current = true;
      setBusy(true);
      setError(null);
      const params: { session_id: string; text: string; images?: WireImage[] } = {
        session_id: id,
        text: trimmed,
      };
      if (images.length > 0) params.images = images;
      void request<{ accepted: boolean }>('turn/submit', params).catch((cause) => {
        turnActive.current = false;
        setBusy(false);
        setError(describe(cause));
      });
    },
    [busy, append],
  );

  const cancel = useCallback(() => {
    const id = sessionIdRef.current;
    if (!id || !busy) return;
    void request<{ accepted: boolean }>('turn/cancel', { session_id: id }).catch((cause) =>
      setError(describe(cause)),
    );
  }, [busy]);

  const answer = useCallback(
    (answer: AskAnswer) => {
      if (!pendingQuestion) return;
      respond(pendingQuestion.id, { answer });
      setPendingQuestion(null);
    },
    [pendingQuestion],
  );

  const dismissQuestion = useCallback(() => {
    // Dropping the request fails the agent's ask fast instead of hanging it.
    setPendingQuestion(null);
  }, []);

  return {
    phase,
    sessionId,
    messages,
    busy,
    error,
    tokenUsage,
    contextWindow,
    pendingQuestion,
    archived,
    active,
    create,
    resume,
    attach,
    closeSession,
    refreshList,
    send,
    cancel,
    answer,
    dismissQuestion,
  };
}

function describe(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}
