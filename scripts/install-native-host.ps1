# Registers the SwiftFetch native-messaging host on Windows (M5 packaging).
# Idempotent; run per-user (no admin required).
$AppId = "app.swiftfetch.desktop"
$Bin = if ($env:SWIFTFETCH_NATIVE_HOST) { $env:SWIFTFETCH_NATIVE_HOST } else { "$env:LOCALAPPDATA\SwiftFetch\swiftfetch-native-host.exe" }
$Manifest = "app_swiftfetch_desktop.json"

function Write-Manifest($dir, $origins) {
  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  $json = "{ `"name`": `"$AppId`", `"description`": `"SwiftFetch browser bridge`", `"path`": `"$($Bin -replace '\\','\\')`", `"type`": `"stdio`", `"allowed_origins`": [$origins] }"
  Set-Content -Path (Join-Path $dir $Manifest) -Value $json -Encoding UTF8
  Write-Output "wrote $dir\$Manifest"
}

$chromeOrigins = '"chrome-extension://__CHROME_ID__/"'
Write-Manifest "$env:LOCALAPPDATA\Google\Chrome\User Data\NativeMessagingHosts" $chromeOrigins
Write-Manifest "$env:LOCALAPPDATA\Microsoft\Edge\User Data\NativeMessagingHosts" $chromeOrigins
Write-Manifest "$env:APPDATA\Mozilla\NativeMessagingHosts" '"firefox@swiftfetch.app"'
Write-Output "native host binary expected at: $Bin"
