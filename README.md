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
- **Per-Vault Groups**: Create, rename or delete groups independently in the local vault and in each WebDAV vault. Hosts in deleted groups appear under Ungrouped. The home board displays hosts from unlocked vaults in separate columns.
- **Optional App Lock**: Set or change the app password separately from vault passwords. The sidebar locks the app manually. Idle locking is off by default and pauses while any RDP tab is open. Middle-click an RDP tab to close it.
- **Zero-Knowledge WebDAV Sync**:
  - Create or open multiple WebDAV vaults from the unlock screen or Settings. Each vault has its own master password, groups, hosts and sync settings. Use Test Connection to check the WebDAV address and credentials.
  - Unlocked WebDAV vaults sync every 10 seconds while the app is unlocked. Each vault runs one sync at a time.
  - Windows stores the local vault under `%ProgramData%\MistRDP\mist-rdp.vault.json`. If the destination is empty, MistRDP copies the old encrypted vault there. The previous WebDAV connection appears as a separate remote vault and is loaded from its server when first unlocked.
  - WebDAV uploads use `If-None-Match: *` for first creation and a strong `ETag` with `If-Match` for updates. MistRDP retries version conflicts after fetching and merging again.
  - Host and group edits merge on-device. Deleted IDs stay deleted when offline devices rejoin. Update every device that writes to the same vault before using managed groups.

## Zero-Knowledge Sync Flow

```text
Device A (Windows) ──┐                                ┌── Device B (macOS)
  Argon2id+AES-GCM   │    Encrypted Envelope Only     │   Argon2id+AES-GCM
  Host / Group Merge ├──────────► WebDAV ◄──────────┤   Host / Group Merge
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
│       ├── profiles.rs        # WebDAV vault registry and isolated caches
│       ├── sync.rs            # Zero-knowledge WebDAV sync engine
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
