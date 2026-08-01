import { useEffect, useState } from 'react';
import type { AskAnswer } from '../../protocol';
import type { PendingQuestion } from '../hooks/useSession';

interface AskModalProps {
  question: PendingQuestion;
  onAnswer: (answer: AskAnswer) => void;
  onDismiss: () => void;
}

/** Minimal ask/question surface: option buttons plus a free-text fallback. */
export function AskModal({ question, onAnswer, onDismiss }: AskModalProps) {
  const [freeText, setFreeText] = useState('');
  const [custom, setCustom] = useState(false);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onDismiss();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onDismiss]);

  const submitFreeText = () => {
    const text = freeText.trim();
    if (!text) return;
    onAnswer({ type: 'free_text', data: text });
  };

  return (
    <div className="ask-overlay">
      <div className="ask-modal">
        <div className="ask-question">{question.question}</div>
        <div className="ask-options">
          {question.options.map((option, index) => (
            <button
              key={option.label}
              className="ask-option"
              onClick={() => onAnswer({ type: 'option', data: { index, label: option.label } })}
            >
              {option.label}
              {option.description && <span className="ask-option-description">{option.description}</span>}
            </button>
          ))}
          {!custom && (
            <button className="ask-option ask-custom-toggle" onClick={() => setCustom(true)}>
              Write my own answer…
            </button>
          )}
          {custom && (
            <div className="ask-free-text">
              <input
                autoFocus
                value={freeText}
                onChange={(event) => setFreeText(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === 'Enter') submitFreeText();
                }}
                placeholder="Type an answer…"
              />
              <div className="ask-free-text-actions">
                <button onClick={submitFreeText} disabled={!freeText.trim()}>
                  Answer
                </button>
              </div>
            </div>
          )}
        </div>
        <div className="ask-dismiss">
          <button className="ask-dismiss-button" onClick={onDismiss}>
            Dismiss
          </button>
        </div>
      </div>
    </div>
  );
}
