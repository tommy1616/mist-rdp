# Security Policy & Zero-Knowledge Architecture

## Cryptography & Vault Model

MistRDP is designed around a **zero-knowledge, client-side encrypted vault**:

- **Key Derivation**: Argon2id (`v0x13`, 19 MiB memory, 2 iterations, 1 lane, 128-bit random salt per vault).
- **Authenticated Encryption**: AES-256-GCM with a fresh random 96-bit nonce generated on every write.
- **No Plaintext Exposure**:
  - Frontend WebView never receives plaintext host passwords (`SavedHostView` exposes only `has_password: bool`).
  - Locking the vault or exiting the application explicitly zeroizes in-memory derived keys and cached host passwords (`zeroize`).
  - Deleted hosts become secretless tombstones (`password: None`) for sync propagation and are pruned after 90 days.
- **OS Keychain Integration (Optional)**:
  - Master password persistence (opt-in) and WebDAV sync passwords are stored exclusively in the operating system's credential vault (Windows Credential Manager / macOS Keychain) via `keyring`.
  - Sync configuration files on disk (`mist-rdp.sync.json`) never store passwords.

## Zero-Knowledge Cross-Device Sync (BYOS)

MistRDP does **not** operate any central user server. Instead, users bring their own storage:

1. **Sync Folder** (iCloud Drive, OneDrive, Dropbox, Google Drive, Syncthing, NAS share)
2. **WebDAV** (Nextcloud, Synology/TrueNAS NAS, self-hosted WebDAV)

Sync guarantees:
- Only the AES-256-GCM encrypted envelope (`mist-rdp.vault.json`) ever leaves the device.
- Merging happens locally after decryption via per-host Last-Write-Wins (LWW) timestamps + soft-delete tombstones, then re-encrypted before writing back.
- Local writes use unique temporary files + `sync_all()` + atomic rename, keeping up to 5 rolling encrypted backups under `.mist-rdp-backups/`.
- WebDAV enforces `https://` for all public hosts (`http://` is restricted to loopback and private LAN addresses), rejects embedded URL credentials (`user:pass@`), restricts remote filenames to URL-safe `[A-Za-z0-9._-]`, disables HTTP redirects (preventing cross-host Basic Auth leakage), enforces a 10 MiB response size cap, and uses `ETag` / `If-Match` optimistic locking with automatic `412` conflict retry.
- **LWW Merge Semantics**: Because merging operates per-host by UTC timestamp, an edit made on another offline device with a newer timestamp than a deletion will supersede the deletion; exact timestamp ties favor deletion.

## Local Framebuffer Channel

The internal WebSocket framebuffer stream binds exclusively to `127.0.0.1` on an ephemeral port and authenticates clients with a per-launch random token verified via constant-time comparison.

## Reporting a Vulnerability

If you discover a security issue, please report it privately via GitHub Security Advisories instead of opening a public issue.
