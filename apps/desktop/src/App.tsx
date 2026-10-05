import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { PingPanel } from "./components/PingPanel";

interface DbStatus {
  path: string;
  journal_mode: string | null;
  tables: number | null;
  error: string | null;
}

interface PingState {
  reply: string;
  autoCount: number;
  manualCount: number;
}

export default function App() {
  const [pingState, setPingState] = useState<PingState>({
    reply: "",
    autoCount: 0,
    manualCount: 0,
  });
  const [db, setDb] = useState<DbStatus | null>(null);

  useEffect(() => {
    invoke<DbStatus>("db_status")
      .then(setDb)
      .catch((err: unknown) =>
        setDb({ path: "", journal_mode: null, tables: null, error: String(err) }),
      );
  }, []);

  const ping = useCallback((via: "auto" | "button") => {
    void invoke<string>("ping")
      .then((reply) => {
        setPingState((prev) => ({
          reply,
          autoCount: prev.autoCount + (via === "auto" ? 1 : 0),
          manualCount: prev.manualCount + (via === "button" ? 1 : 0),
        }));
      })
      .catch((err: unknown) => {
        setPingState((prev) => ({ ...prev, reply: `error: ${String(err)}` }));
      });
  }, []);

  // One round-trip on mount so the scaffold self-demonstrates the IPC path.
  useEffect(() => {
    const timer = window.setTimeout(() => ping("auto"), 600);
    return () => window.clearTimeout(timer);
  }, [ping]);

  return (
    <main className="shell">
      <header className="header">
        <span className="logo" aria-hidden="true" />
        <div>
          <h1>SwiftFetch</h1>
          <p className="muted">High-speed download manager · Milestone 0 scaffold</p>
        </div>
      </header>

      <div className="cards">
        <PingPanel
          reply={pingState.reply}
          autoCount={pingState.autoCount}
          manualCount={pingState.manualCount}
          onPing={() => ping("button")}
        />

        <section className="card">
          <h2>SQLite store</h2>
          <p className="muted">
            WAL-mode database created at the OS app-data directory on first run.
          </p>
          {db === null && <p className="muted">checking…</p>}
          {db !== null && db.error === null && (
            <ul className="kv">
              <li>
                <span className="muted">path</span>
                <code>{db.path}</code>
              </li>
              <li>
                <span className="muted">journal</span>
                <span className={db.journal_mode === "wal" ? "badge ok" : "badge warn"}>
                  {db.journal_mode ?? "unknown"}
                  {db.journal_mode === "wal" ? " ✓" : ""}
                </span>
              </li>
              <li>
                <span className="muted">tables</span>
                <span>{db.tables ?? "?"}</span>
              </li>
            </ul>
          )}
          {db !== null && db.error !== null && (
            <p className="badge warn">store error: {db.error}</p>
          )}
        </section>
      </div>

      <footer className="muted small">
        Scaffold milestone — the download engine lands in Milestone 1, browser capture in
        Milestone 4.
      </footer>
    </main>
  );
}
