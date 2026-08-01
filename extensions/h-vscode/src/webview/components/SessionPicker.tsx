import type { ArchivedSession } from '../../protocol';

interface SessionPickerProps {
  archived: ArchivedSession[];
  active: { id: string }[];
  error: string | null;
  onCreate: () => void;
  onResume: (id: string) => void;
  onAttach: (id: string) => void;
  onClose: (id: string) => void;
  onRefresh: () => void;
  /** Present when the picker opens over a live chat. */
  onBack?: () => void;
}

/** Active/archived session list with create/resume, mirroring the TUI picker. */
export function SessionPicker({
  archived,
  active,
  error,
  onCreate,
  onResume,
  onAttach,
  onClose,
  onRefresh,
  onBack,
}: SessionPickerProps) {
  return (
    <div className="picker">
      <div className="picker-head">
        <span className="picker-title">h</span>
        {onBack && (
          <button className="picker-back" onClick={onBack}>
            Back to chat
          </button>
        )}
        <button className="picker-refresh" onClick={onRefresh} title="Refresh session list">
          ⟳
        </button>
      </div>

      {error && <div className="error">{error}</div>}

      <button className="picker-new" onClick={onCreate}>
        + New session
      </button>

      {active.length > 0 && (
        <section className="picker-section">
          <h2>Active</h2>
          {active.map((session) => (
            <div key={session.id} className="picker-row">
              <span className="picker-id">{shortId(session.id)}</span>
              <span className="picker-actions">
                <button onClick={() => onAttach(session.id)}>Open</button>
                <button className="picker-danger" onClick={() => onClose(session.id)}>
                  Close
                </button>
              </span>
            </div>
          ))}
        </section>
      )}

      <section className="picker-section">
        <h2>Archived</h2>
        {archived.length === 0 ? (
          <p className="picker-empty">No archived sessions yet.</p>
        ) : (
          archived.map((session) => (
            <div key={session.id} className="picker-row">
              <span className="picker-meta">
                <span className="picker-title-text">{session.title || shortId(session.id)}</span>
                <span className="picker-sub">{shortId(session.id)} · {session.last_modified}</span>
              </span>
              <button onClick={() => onResume(session.id)}>Resume</button>
            </div>
          ))
        )}
      </section>
    </div>
  );
}

function shortId(id: string): string {
  return id.length > 8 ? `${id.slice(0, 8)}…` : id;
}
