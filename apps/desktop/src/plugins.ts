/**
 * SwiftFetch plugin hooks (Milestone 6, Build Prompt §14.3).
 *
 * A tiny pub/sub surface over download lifecycle events. Plugins register
 * handlers; the app emits. See `examples/on-complete-notify` for a minimal
 * plugin built against this API.
 *
 * ```ts
 * import { onDownloadComplete, onQueueEmpty } from "./plugins";
 *
 * const offComplete = onDownloadComplete(({ id, filename }) => {
 *   console.log(`done: ${filename} (${id})`);
 * });
 * const offEmpty = onQueueEmpty((queueId) => {
 *   console.log(`queue drained: ${queueId}`);
 * });
 * // Later: offComplete(); offEmpty();
 * ```
 */

export interface DownloadCompletedInfo {
  /** SwiftFetch download id. */
  id: string;
  /** File name at completion time (best effort — may be empty). */
  filename: string;
}

export type DownloadCompleteHandler = (info: DownloadCompletedInfo) => void;
export type QueueEmptyHandler = (queueId: string) => void;

const completeHandlers = new Set<DownloadCompleteHandler>();
const queueEmptyHandlers = new Set<QueueEmptyHandler>();

/** Registers a download-completion handler. Returns an unsubscribe function. */
export function onDownloadComplete(handler: DownloadCompleteHandler): () => void {
  completeHandlers.add(handler);
  return () => {
    completeHandlers.delete(handler);
  };
}

/** Registers a queue-drained handler. Returns an unsubscribe function. */
export function onQueueEmpty(handler: QueueEmptyHandler): () => void {
  queueEmptyHandlers.add(handler);
  return () => {
    queueEmptyHandlers.delete(handler);
  };
}

/** Emits a completion (called by the app's event bridge — not by plugins). */
export function emitDownloadComplete(info: DownloadCompletedInfo): void {
  for (const handler of [...completeHandlers]) {
    try {
      handler(info);
    } catch (err) {
      console.error("plugin onDownloadComplete handler failed", err);
    }
  }
}

/** Emits a queue-drained event (called by the app — not by plugins). */
export function emitQueueEmpty(queueId: string): void {
  for (const handler of [...queueEmptyHandlers]) {
    try {
      handler(queueId);
    } catch (err) {
      console.error("plugin onQueueEmpty handler failed", err);
    }
  }
}
