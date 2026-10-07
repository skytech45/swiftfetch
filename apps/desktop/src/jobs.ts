import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";

export interface JobView {
  id: string;
  filename: string;
  url: string;
  state: string;
  doneBytes: number;
  totalLen: number | null;
  speedBps: number;
  categoryId: string | null;
  queueId: string | null;
  errorCode: string | null;
  errorMsg: string | null;
  createdAt: string;
}

export interface CategoryView {
  id: string;
  name: string;
  extensions: string[];
  folder: string;
}

export interface QueueView {
  id: string;
  name: string;
  maxConcurrent: number;
  isActive: boolean;
  jobIds: string[];
  /** Scheduler JSON (M3); null = manual queue. */
  scheduleJson: string | null;
  /** Post-drain action: none | sleep | hibernate | shutdown. */
  postAction: string;
}

export interface QuotaStatusView {
  hourlyLimit: number | null;
  dailyLimit: number | null;
  hourlyUsed: number;
  dailyUsed: number;
  exhausted: boolean;
}

export interface SegmentView {
  idx: number;
  start: number;
  end: number;
  done: number;
  state: string;
}

interface DownloadEvent {
  jobId: string;
  kind: "progress" | "state" | "completed" | "failed";
}

/** Live jobs list: initial load, then event-driven refresh (≤1 Hz). */
export function useJobs(): JobView[] {
  const [jobs, setJobs] = useState<JobView[]>([]);

  useEffect(() => {
    let dirty = false;
    const load = () => {
      invoke<JobView[]>("list_jobs")
        .then(setJobs)
        .catch(() => {});
    };
    load();
    const interval = window.setInterval(() => {
      if (dirty) {
        dirty = false;
        load();
      }
    }, 500);
    const unlisten = window.__swiftfetchOnEvent(() => {
      dirty = true;
    });
    return () => {
      window.clearInterval(interval);
      void unlisten;
    };
  }, []);

  return jobs;
}

// Minimal typed bridge to the Tauri event API (avoid pulling the whole
// @tauri-apps/api/event surface into components).
declare global {
  interface Window {
    __swiftfetchOnEvent: (
      handler: (event: DownloadEvent) => void,
    ) => () => void;
  }
}

export async function installEventBridge(): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  const unlisten = await listen<DownloadEvent>("download://event", (e) => {
    for (const handler of handlers) handler(e.payload);
  });
  return () => {
    unlisten();
  };
}

const handlers = new Set<(event: DownloadEvent) => void>();
window.__swiftfetchOnEvent = (handler) => {
  handlers.add(handler);
  return () => handlers.delete(handler);
};

// ── formatting helpers ───────────────────────────────────────────────────

export function formatBytes(bytes: number | null): string {
  if (bytes === null) return "—";
  const units = ["B", "KiB", "MiB", "GiB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}

export function formatSpeed(bps: number): string {
  if (bps <= 0) return "—";
  return `${formatBytes(bps)}/s`;
}

export function formatEta(done: number, total: number | null, bps: number): string {
  if (total === null || bps <= 0 || done >= total) return "—";
  const secs = Math.round((total - done) / bps);
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}

export { invoke };
