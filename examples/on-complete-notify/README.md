# on-complete-notify — example SwiftFetch plugin

Minimal plugin built against the public hooks API
(`apps/desktop/src/plugins.ts`, Milestone 6):

- `onDownloadComplete(handler)` — fires with `{ id, filename }` for every
  completed download. Returns an unsubscribe function.
- `onQueueEmpty(handler)` — fires with the queue id when a queue's active
  count drops to zero. Returns an unsubscribe function.

Both emitters isolate handler failures (one throwing plugin never breaks
the app or other plugins).

## Check it

```sh
cd examples/on-complete-notify
../apps/desktop/node_modules/.bin/tsc --noEmit --strict --skipLibCheck \
  --module nodenext --moduleResolution nodenext --target es2022 index.ts
```
