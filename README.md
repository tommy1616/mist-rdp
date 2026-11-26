# MistRDP

Cross-platform (Windows & macOS Apple Silicon) multi-tab RDP client built with **Rust + Tauri v2 + pure Rust IronRDP**. Zero C/OpenSSL external dependencies, native keyboard shortcut capture, bidirectional clipboard & audio, local folder redirection, and a **zero-knowledge encrypted credential vault with BYOS cross-device sync**.

## Core Features

- **Multi-Tab Concurrent RDP Sessions**: Connect to multiple Windows hosts simultaneously, double-click host cards to connect, and switch tabs without dropping sessions.
- **Auto-Hide Floating HUD & True Fullscreen**:
  - Top HUD automatically collapses into a minimalist pill handle during sessions (with an optional Pin toggle).
  - One-click true fullscreen hides the custom titlebar and stays synced with native OS fullscreen transitions.
  - Dynamic resolution negotiation (`DRDYNVC` Display Control) automatically fits the remote desktop to window/fullscreen resizing.
- **Native System Shortcut Capture & Stuck-Key Protection**:
  - Windows `WH_KEYBOARD_LL` hook forwards `Win`, `Win+E/R/D/L`, `Alt+Tab`, `Alt+F4`, `Ctrl+Esc`, and `PrintScreen` (`Ctrl+Alt+Pause` emergency release). In remote-shortcut mode, `Ctrl+Alt+Tab` switches local windows instead (local `Alt+Tab`).
  - macOS `CGEventTap` captures system hotkeys with optional `Command → Ctrl` mapping (`Ctrl+Option+Esc` emergency release).
  - Automatic modifier key release (LCtrl/RCtrl/LAlt/RAlt/LShift/RShift/LWin/RWin) on focus loss or tab switch.
- **Bidirectional Clipboard**:
  - Windows native OLE clipboard (text, images, file copy/paste).
  - macOS text + bidirectional PNG ↔ `CF_DIB` image sync using in-process `NSPasteboard.changeCount` polling (zero idle subprocess overhead).
- **Remote Audio Playback**: Pure Rust `RDPSND` + `AUDIO_PLAYBACK_DVC` audio output (PCM / ADPCM / G.711 via `cpal`), configurable per host.
- **Local Folder Sharing (RDPDR)**: Share any local folder (with Desktop / Downloads / Documents quick presets) directly into the remote Windows session as a redirected drive.
- **Zero-Knowledge Encrypted Vault (`mist-rdp.vault.json`)**:
  - **Argon2id + AES-256-GCM** client-side encryption with atomic writes and up to 5 rolling local backups.
  - Optional OS Keychain master password persistence (Windows Credential Manager / macOS Keychain).
  - Per-host option to save password or prompt on every connection.
- **Zero-Knowledge Cross-Device Sync (BYOS — Bring Your Own Storage)**:
  - **Sync Folder**: Point directly to iCloud Drive, OneDrive, Dropbox, Google Drive, Syncthing, or a mounted NAS folder.
  - **WebDAV**: Sync directly with Nextcloud, Synology/TrueNAS NAS, or any standard WebDAV endpoint (WebDAV password stored exclusively in OS Keychain, with `ETag`/`If-Match` optimistic locking and automatic `412` conflict retry).
  - **Per-Host LWW Merge + Tombstones**: Decryption and merging happen strictly on-device—remote storage only ever sees AES-256-GCM ciphertext.

## Zero-Knowledge Sync Flow

```text
Device A (Windows) ──┐                                ┌── Device B (macOS)
  Argon2id+AES-GCM   │    Encrypted Envelope Only     │   Argon2id+AES-GCM
  Per-Host LWW Merge ├──► iCloud / OneDrive / WebDAV ◄┤   Per-Host LWW Merge
  OS Keychain Secret ┘      (Zero Plaintext Ever)     └── OS Keychain Secret
```

See [SECURITY.md](SECURITY.md) for the full cryptographic and threat model.

## Project Structure

```text
mist-rdp/
├── .github/workflows/ci.yml   # Windows + macOS CI (cargo test + JS syntax check)
├── frontend/                  # Mist Studio Web UI (HTML / CSS / JS, zero bundler)
│   ├── index.html
│   ├── style.css
│   └── app.js
├── src-tauri/                 # Tauri v2 + IronRDP Rust backend
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   └── src/
│       ├── main.rs            # Tauri commands & window lifecycle
│       ├── rdp.rs             # Multi-session IronRDP engine (display, audio, clipboard, RDPDR)
│       ├── vault.rs           # Argon2id + AES-256-GCM vault, LWW merge & rolling backups
│       ├── sync.rs            # Zero-knowledge Folder & WebDAV sync engine
│       ├── ws_stream.rs       # Loopback binary RGBA dirty-rect stream (constant-time token auth)
│       ├── keyboard_hook.rs   # Windows WH_KEYBOARD_LL / macOS CGEventTap
│       ├── mac_clipboard.rs   # macOS NSPasteboard text & image sync
│       └── dib.rs             # Pure Rust BMP <-> Windows CF_DIB converter
├── LICENSE                    # MIT License
└── SECURITY.md                # Zero-knowledge security model
```

## Building from Source

### Prerequisites

- Rust `1.94+` (stable toolchain)
- **Windows**: Built-in WebView2 runtime
- **macOS**: Xcode Command Line Tools (`xcode-select --install`)

### Build Commands

```bash
cd src-tauri
cargo test --bins
cargo build --release
```

- **Windows** produces a portable standalone binary at `src-tauri/target/release/mist-rdp.exe`.
- **macOS** produces `src-tauri/target/release/mist-rdp` (can be wrapped in an `.app` bundle or packaged via `cargo tauri build`).

> **macOS Accessibility Permission**: To capture system hotkeys such as `Cmd+Tab` inside RDP sessions, allow MistRDP under **System Settings → Privacy & Security → Accessibility**.

## License

MIT — see [LICENSE](LICENSE).
