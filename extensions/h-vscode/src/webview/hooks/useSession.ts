import { useCallback, useEffect, useRef, useState } from 'react';
import type { AskAnswer, AskQuestionParams, SessionEventParams } from '../../protocol';
import { onNotification, onRequest, request, respond } from '../rpc';

export interface ChatMessage {
  id: number;
  role: 'user' | 'assistant';
  text: string;
}

export interface PendingQuestion {
  id: number;
  question: string;
  options: { label: string; description?: string }[];
}

export interface Session {
  sessionId: string | null;
  messages: ChatMessage[];
  busy: boolean;
  error: string | null;
  pendingQuestion: PendingQuestion | null;
  send: (text: string) => void;
  cancel: () => void;
  answer: (answer: AskAnswer) => void;
  dismissQuestion: () => void;
}

/**
 * One chat session for the panel: creates the session on mount, streams
 * `session/event` notifications into the message list, and answers
 * `ask/question` requests from the server.
 */
export function useSession(): Session {
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pendingQuestion, setPendingQuestion] = useState<PendingQuestion | null>(null);

  const nextMessageId = useRef(1);
  const turnActive = useRef(false);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const created = await request<{ session_id: string }>('session/create', {});
        if (!cancelled) setSessionId(created.session_id);
      } catch (cause) {
        if (!cancelled) setError(describe(cause));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    return onNotification('session/event', (params) => {
      const { event } = params as SessionEventParams;
      handleEvent(event);
    });
  }, []);

  useEffect(() => {
    return onRequest('ask/question', (params, id) => {
      const { question, options } = params as AskQuestionParams;
      setPendingQuestion({ id, question, options });
    });
  }, []);

  const handleEvent = useCallback((event: SessionEventParams['event']) => {
    switch (event.type) {
      case 'text_delta': {
        setMessages((previous) => {
          const last = previous[previous.length - 1];
          if (turnActive.current && last && last.role === 'assistant') {
            return [...previous.slice(0, -1), { ...last, text: last.text + event.data }];
          }
          turnActive.current = true;
          return [...previous, { id: nextMessageId.current++, role: 'assistant', text: event.data }];
        });
        break;
      }
      case 'prompt': {
        // Only emitted on replay (attach); the live path echoes locally.
        setMessages((previous) => [
          ...previous,
          { id: nextMessageId.current++, role: 'user', text: event.data },
        ]);
        break;
      }
      case 'turn_finished':
      case 'error': {
        turnActive.current = false;
        setBusy(false);
        if (event.type === 'error') setError(event.data);
        break;
      }
      default:
        break;
    }
  }, []);

  const send = useCallback(
    (text: string) => {
      const trimmed = text.trim();
      if (!sessionId || !trimmed || busy) return;
      setMessages((previous) => [
        ...previous,
        { id: nextMessageId.current++, role: 'user', text: trimmed },
      ]);
      turnActive.current = true;
      setBusy(true);
      setError(null);
      void request('turn/submit', { session_id: sessionId, text: trimmed }).catch((cause) => {
        turnActive.current = false;
        setBusy(false);
        setError(describe(cause));
      });
    },
    [sessionId, busy],
  );

  const cancel = useCallback(() => {
    if (!sessionId || !busy) return;
    void request('turn/cancel', { session_id: sessionId }).catch((cause) => setError(describe(cause)));
  }, [sessionId, busy]);

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
    sessionId,
    messages,
    busy,
    error,
    pendingQuestion,
    send,
    cancel,
    answer,
    dismissQuestion,
  };
}

function describe(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}
