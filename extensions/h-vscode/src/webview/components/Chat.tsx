import { useEffect, useRef, useState, type ClipboardEvent, type FormEvent, type KeyboardEvent } from 'react';
import type { WireImage } from '../../protocol';
import type { ChatMessage } from '../hooks/useSession';
import { Markdown } from './Markdown';
import { ToolCard } from './ToolCard';

interface ChatProps {
  messages: ChatMessage[];
  busy: boolean;
  error: string | null;
  onSend: (text: string, images?: WireImage[]) => void;
  onCancel: () => void;
}

const COMMANDS = [
  { command: '/clear', description: 'Start a fresh session, archiving the current one' },
  { command: '/compact', description: 'Compact the conversation context' },
];

interface Attachment {
  mediaType: string;
  data: string;
  width: number;
  height: number;
}

export function Chat({ messages, busy, error, onSend, onCancel }: ChatProps) {
  const [draft, setDraft] = useState('');
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [highlight, setHighlight] = useState(0);
  const bottomRef = useRef<HTMLDivElement>(null);

  const suggestions = draft.startsWith('/')
    ? COMMANDS.filter((command) => command.command.startsWith(draft) && command.command !== draft)
    : [];
  const menuOpen = suggestions.length > 0;

  useEffect(() => {
    setHighlight((current) => (menuOpen ? Math.min(current, suggestions.length - 1) : 0));
  }, [draft, menuOpen, suggestions.length]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, busy]);

  const pickCommand = (command: string) => {
    setDraft(`${command} `);
    setHighlight(0);
  };

  const submit = (event?: FormEvent) => {
    event?.preventDefault();
    const text = draft.trim();
    if ((!text && attachments.length === 0) || busy) return;
    onSend(
      text,
      attachments.length > 0
        ? attachments.map(({ mediaType, data, width, height }) => ({ media_type: mediaType, data, width, height }))
        : undefined,
    );
    setDraft('');
    setAttachments([]);
    setHighlight(0);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (menuOpen) {
      switch (event.key) {
        case 'ArrowDown':
          event.preventDefault();
          setHighlight((current) => Math.min(current + 1, suggestions.length - 1));
          return;
        case 'ArrowUp':
          event.preventDefault();
          setHighlight((current) => Math.max(current - 1, 0));
          return;
        case 'Enter':
        case 'Tab':
          event.preventDefault();
          pickCommand(suggestions[highlight].command);
          return;
        case 'Escape':
          event.preventDefault();
          setDraft('');
          setAttachments([]);
          return;
      }
    }
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      submit();
    }
  };

  const onPaste = (event: ClipboardEvent<HTMLTextAreaElement>) => {
    const files = [...event.clipboardData.items]
      .filter((item) => item.kind === 'file' && item.type.startsWith('image/'))
      .map((item) => item.getAsFile())
      .filter((file): file is File => file !== null);
    if (files.length === 0) return;
    event.preventDefault();
    for (const file of files) void addImageAttachment(file);
  };

  const removeAttachment = (index: number) => {
    setAttachments((previous) => previous.filter((_, i) => i !== index));
  };

  const addImageAttachment = async (file: File) => {
    try {
      const dataUrl = await readAsDataURL(file);
      const match = /^data:([^;]+);base64,(.*)$/s.exec(dataUrl);
      if (!match) return;
      const mediaType = match[1];
      const data = match[2];
      const { width, height } = await imageDimensions(dataUrl);
      setAttachments((previous) => [...previous, { mediaType, data, width, height }]);
    } catch (cause) {
      console.error('image paste failed', cause);
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
                  message.role === 'assistant' ? (
                    <Markdown key={index} text={block.text} />
                  ) : (
                    <div key={index} className="message-text">
                      {block.text}
                    </div>
                  )
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
        {attachments.length > 0 && (
          <div className="attachments">
            {attachments.map((attachment, index) => (
              <div key={index} className="attachment">
                <img
                  src={`data:${attachment.mediaType};base64,${attachment.data}`}
                  alt="Pasted image"
                  width={attachment.width}
                  height={attachment.height}
                />
                <button
                  className="attachment-remove"
                  onClick={() => removeAttachment(index)}
                  title="Remove image"
                >
                  ×
                </button>
              </div>
            ))}
          </div>
        )}
        {menuOpen && (
          <div className="command-menu">
            {suggestions.map((command, index) => (
              <button
                key={command.command}
                type="button"
                className={`command-item ${index === highlight ? 'selected' : ''}`}
                onMouseDown={(event) => {
                  event.preventDefault();
                  pickCommand(command.command);
                }}
                onMouseEnter={() => setHighlight(index)}
              >
                <span className="command-name">{command.command}</span>
                <span className="command-description">{command.description}</span>
              </button>
            ))}
          </div>
        )}
        <textarea
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={onKeyDown}
          onPaste={onPaste}
          placeholder="Message h… (enter to send, /clear, /compact)"
          rows={2}
        />
        <div className="composer-actions">
          {busy ? (
            <button type="button" onClick={onCancel}>
              Cancel
            </button>
          ) : (
            <button type="submit" disabled={!draft.trim() && attachments.length === 0}>
              Send
            </button>
          )}
        </div>
      </form>
    </div>
  );
}

function readAsDataURL(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}

function imageDimensions(dataUrl: string): Promise<{ width: number; height: number }> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.onload = () => resolve({ width: image.naturalWidth, height: image.naturalHeight });
    image.onerror = () => reject(new Error('could not decode pasted image'));
    image.src = dataUrl;
  });
}
