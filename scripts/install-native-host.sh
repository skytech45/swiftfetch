#!/usr/bin/env bash
# Registers the SwiftFetch native-messaging host for Chrome/Edge/Opera and
# Firefox (M5 packaging). Idempotent; prints what it wrote.
set -euo pipefail

APP_ID="app.swiftfetch.desktop"
BIN="${SWIFTFETCH_NATIVE_HOST:-$HOME/.local/bin/swiftfetch-native-host}"
MANIFEST_NAME="app_swiftfetch_desktop.json"

write_manifest() {
  local dir="$1" allowed="$2"
  mkdir -p "$dir"
  cat > "$dir/$MANIFEST_NAME" <<EOF
{
  "name": "$APP_ID",
  "description": "SwiftFetch browser bridge",
  "path": "$BIN",
  "type": "stdio",
  "allowed_origins": [$allowed]
}
EOF
  echo "wrote $dir/$MANIFEST_NAME"
}

CHROME_ORIGINS='"chrome-extension://__CHROME_ID__/"'
FIREFOX_ORIGINS='"firefox@swiftfetch.app"'

write_manifest "$HOME/.config/google-chrome/NativeMessagingHosts" "$CHROME_ORIGINS"
write_manifest "$HOME/.config/microsoft-edge/NativeMessagingHosts" "$CHROME_ORIGINS"
write_manifest "$HOME/.mozilla/native-messaging-hosts" "$FIREFOX_ORIGINS"

echo "native host binary expected at: $BIN"
