/**
 * Example SwiftFetch plugin (Milestone 6, Build Prompt §14.3).
 *
 * Built against the public hooks API in `apps/desktop/src/plugins.ts`:
 * it logs every completed download and every drained queue. A real plugin
 * would show an OS notification or POST to a webhook here.
 *
 * Type-check it with the desktop workspace compiler:
 * `../apps/desktop/node_modules/.bin/tsc --noEmit --strict --skipLibCheck
 * --module nodenext --moduleResolution nodenext --target es2022 index.ts`
 */
import {
  onDownloadComplete,
  onQueueEmpty,
} from "../../apps/desktop/src/plugins";

const offComplete = onDownloadComplete(({ id, filename }) => {
  console.log(`[notify] download complete: ${filename} (${id})`);
});

const offEmpty = onQueueEmpty((queueId) => {
  console.log(`[notify] queue drained: ${queueId}`);
});

// Exported so hosts (and tests) can unload the plugin cleanly.
export function unload(): void {
  offComplete();
  offEmpty();
}
