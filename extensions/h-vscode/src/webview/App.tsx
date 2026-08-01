import { AskModal } from './components/AskModal';
import { Chat } from './components/Chat';
import { useSession } from './hooks/useSession';

export default function App() {
  const session = useSession();

  return (
    <div className="app">
      <header className="header">
        <span className="title">h</span>
        {session.sessionId && <span className="session-id">{shortId(session.sessionId)}</span>}
        {session.busy && <span className="busy">working…</span>}
      </header>
      <Chat
        messages={session.messages}
        busy={session.busy}
        error={session.error}
        onSend={session.send}
        onCancel={session.cancel}
      />
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
