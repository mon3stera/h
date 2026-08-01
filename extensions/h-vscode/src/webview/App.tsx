import { useState } from 'react';
import { AskModal } from './components/AskModal';
import { Chat } from './components/Chat';
import { SessionPicker } from './components/SessionPicker';
import { StatusBar } from './components/StatusBar';
import { useSession } from './hooks/useSession';

export default function App() {
  const session = useSession();
  const [pickerOpen, setPickerOpen] = useState(false);

  if (session.phase === 'loading') {
    return (
      <div className="app center">
        <span className="busy">h…</span>
      </div>
    );
  }

  if (session.phase === 'idle') {
    return (
      <SessionPicker
        archived={session.archived}
        active={session.active}
        error={session.error}
        onCreate={session.create}
        onResume={session.resume}
        onAttach={session.attach}
        onClose={session.closeSession}
        onRefresh={session.refreshList}
      />
    );
  }

  return (
    <div className="app">
      <header className="header">
        <span className="title">h</span>
        {session.sessionId && <span className="session-id">{shortId(session.sessionId)}</span>}
        <StatusBar usage={session.tokenUsage} />
        {session.busy && <span className="busy">working…</span>}
        <span className="header-spacer" />
        <button className="header-button" onClick={() => setPickerOpen(true)}>
          Sessions
        </button>
      </header>
      <Chat
        messages={session.messages}
        busy={session.busy}
        error={session.error}
        onSend={session.send}
        onCancel={session.cancel}
      />
      {pickerOpen && (
        <div className="picker-overlay">
          <SessionPicker
            archived={session.archived}
            active={session.active}
            error={session.error}
            onCreate={() => {
              setPickerOpen(false);
              session.create();
            }}
            onResume={(id) => {
              setPickerOpen(false);
              session.resume(id);
            }}
            onAttach={(id) => {
              setPickerOpen(false);
              session.attach(id);
            }}
            onClose={session.closeSession}
            onRefresh={session.refreshList}
            onBack={() => setPickerOpen(false)}
          />
        </div>
      )}
      {session.pendingQuestion && (
        <AskModal
          question={session.pendingQuestion}
          onAnswer={session.answer}
          onDismiss={session.dismissQuestion}
        />
      )}
    </div>
  );
}

function shortId(id: string): string {
  return id.length > 8 ? `${id.slice(0, 8)}…` : id;
}
