import { useEffect, useRef, useState, type FormEvent, type KeyboardEvent } from 'react';
import type { ChatMessage } from '../hooks/useSession';
import { ToolCard } from './ToolCard';

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
        {messages.map((message) =>
          message.role === 'system' ? (
            <div key={message.id} className="system-message">
              {message.content.map((block) => (block.kind === 'text' ? block.text : '')).join('')}
            </div>
          ) : (
            <div key={message.id} className={`message ${message.role}`}>
              {message.content.map((block, index) =>
                block.kind === 'text' ? (
                  <div key={index} className="message-text">
                    {block.text}
                  </div>
                ) : (
                  <ToolCard key={index} tool={block.tool} />
                ),
              )}
            </div>
          ),
        )}
        {busy && <div className="message assistant typing">…</div>}
        {error && <div className="error">{error}</div>}
        <div ref={bottomRef} />
      </div>
      <form className="composer" onSubmit={submit}>
        <textarea
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={onKeyDown}
          placeholder="Message h… (enter to send, /clear, /compact)"
          rows={2}
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
