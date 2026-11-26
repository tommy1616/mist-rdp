# Security Policy & Zero-Knowledge Architecture

## Cryptography & Vault Model

MistRDP is designed around a **zero-knowledge, client-side encrypted vault**:

- **Key Derivation**: Argon2id (`v0x13`, 19 MiB memory, 2 iterations, 1 lane, 128-bit random salt per vault).
- **Authenticated Encryption**: AES-256-GCM with a fresh random 96-bit nonce generated on every write.
- **No Plaintext Exposure**:
  - Frontend WebView never receives plaintext host passwords (`SavedHostView` exposes only `has_password: bool`).
  - Locking the vault or exiting the application explicitly zeroizes in-memory derived keys and cached host passwords (`zeroize`).
  - Deleted hosts become secretless tombstones (`password: None`). Host and group tombstones are retained so long-offline devices cannot revive deleted IDs.
- **OS Keychain Integration (Optional)**:
  - Master password persistence (opt-in) and WebDAV sync passwords are stored exclusively in the operating system's credential vault (Windows Credential Manager / macOS Keychain) via `keyring`.
  - Sync configuration files on disk (`mist-rdp.sync.json`) never store passwords.

## Zero-Knowledge Cross-Device Sync (BYOS)

MistRDP does **not** operate any central user server. Instead, users bring their own storage:

- **WebDAV** (Nextcloud, Synology/TrueNAS NAS, self-hosted WebDAV)

Sync guarantees:
- Only the AES-256-GCM encrypted envelope (`mist-rdp.vault.json`) ever leaves the device.
- Hosts and groups merge by stable ID after local decryption. The newest edit wins between live records, and soft deletion takes precedence over edits. The merged vault is encrypted before upload.
- Local writes use unique temporary files + `sync_all()` + atomic rename, keeping up to 5 rolling encrypted backups under `.mist-rdp-backups/`.
- WebDAV enforces `https://` for all public hosts (`http://` is restricted to loopback and private LAN addresses), rejects embedded URL credentials (`user:pass@`), restricts remote filenames to URL-safe `[A-Za-z0-9._-]`, disables HTTP redirects, and caps vault transfers at 10 MiB. First uploads require `If-None-Match: *`, and updates require a strong `ETag` with `If-Match`. MistRDP fetches and merges again after a `412`.

## Local Framebuffer Channel

The internal WebSocket framebuffer stream binds exclusively to `127.0.0.1` on an ephemeral port and authenticates clients with a per-launch random token verified via constant-time comparison.

## Reporting a Vulnerability

If you discover a security issue, please report it privately via GitHub Security Advisories instead of opening a public issue.
