interface PingPanelProps {
  reply: string;
  autoCount: number;
  manualCount: number;
  onPing: () => void;
}

export function PingPanel({ reply, autoCount, manualCount, onPing }: PingPanelProps) {
  return (
    <section className="card">
      <h2>Engine round-trip</h2>
      <p className="muted">Invokes the Rust ping command over Tauri IPC.</p>
      <button type="button" className="primary" onClick={onPing}>
        Ping engine
      </button>
      <div className="result" aria-live="polite">
        {reply ? (
          <>
            Rust says: <code>{reply}</code>
          </>
        ) : (
          <span className="muted">No response yet.</span>
        )}
      </div>
      <p className="muted small">
        invocations — auto: {autoCount} · button: {manualCount}
      </p>
    </section>
  );
}
