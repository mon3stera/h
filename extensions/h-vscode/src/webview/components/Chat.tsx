import { useEffect, useRef, useState, type FormEvent, type KeyboardEvent } from 'react';
import type { ChatMessage } from '../hooks/useSession';

interface ChatProps {
  messages: ChatMessage[];
  busy: boolean;
  error: string | null;
  onSend: (text: string) => void;
  onCancel: () => void;
}

export function Chat({ messages, busy, error, onSend, onCancel }: ChatProps) {
  const [draft, setDraft] = useState('');
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, busy]);

  const submit = (event?: FormEvent) => {
    event?.preventDefault();
    const text = draft.trim();
    if (!text || busy) return;
    onSend(text);
    setDraft('');
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      submit();
    }
  };

  return (
    <div className="chat">
      <div className="messages">
        {messages.map((message) => (
          <div key={message.id} className={`message ${message.role}`}>
            {message.text}
          </div>
        ))}
        {busy && <div className="message assistant typing">…</div>}
        {error && <div className="error">{error}</div>}
        <div ref={bottomRef} />
      </div>
      <form className="composer" onSubmit={submit}>
        <textarea
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={onKeyDown}
          placeholder="Message h…"
          rows={2}
          disabled={!busy ? false : undefined}
        />
        <div className="composer-actions">
          {busy ? (
            <button type="button" onClick={onCancel}>
              Cancel
            </button>
          ) : (
            <button type="submit" disabled={!draft.trim()}>
              Send
            </button>
          )}
        </div>
      </form>
    </div>
  );
}
