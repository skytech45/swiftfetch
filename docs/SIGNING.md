# SwiftFetch Release Signing (Windows production)

Two independent signatures protect users:

1. **Updater signatures (Ed25519, done):** keypair generated
   (`~/.tauri/swiftfetch.key` — private, never committed; public key is
   pinned in `tauri.conf.json`). Every `tauri build` signs artifacts when
   `TAURI_SIGNING_PRIVATE_KEY` is set. The updater refuses anything else.
2. **Windows code signing (needs an account):** removes SmartScreen
   warnings. Recommended: **Azure Trusted Signing** (pay-per-sign, no
   certificate to buy or guard) — or a standard EV/OV cert if preferred.

## One-time setup

1. Azure: create a Trusted Signing account + certificate profile
   (Microsoft docs: "Trusted Signing quickstart").
2. GitHub repo → Settings → Secrets → Actions, add:
   - `TAURI_SIGNING_PRIVATE_KEY` — contents of `~/.tauri/swiftfetch.key`
   - `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`
   - `AZURE_TRUSTED_SIGN_ACCOUNT`, `AZURE_TRUSTED_SIGN_PROFILE`,
     `AZURE_TRUSTED_SIGN_ENDPOINT`
3. Set the admin feed URL: `tauri.conf.json → plugins.updater.endpoints`
   must point at the deployed admin app
   (`https://<admin>/api/updates/stable/{{target}}/{{current_version}}`),
   then flip `active` to `true`.

## Cutting a release

1. Publish notes + artifacts in the admin dashboard (Releases → Publish).
   The updater feed goes live immediately.
2. `git tag v0.2.0; git push origin v0.2.0` — the `release` workflow
   builds the NSIS installer, signs the updater artifacts with the pinned
   key, Trusted-Signs the exe/installer, and attaches everything to the
   GitHub release.
3. The desktop updater picks it up (prompt, or forced below min-required).

## Key custody

- The updater private key signs releases. Back it up offline (two copies,
  two places). Rotation = new keypair + `pubkey` update + app release.
- Never commit keys. Never paste the private key anywhere except the
  GitHub secret above.
