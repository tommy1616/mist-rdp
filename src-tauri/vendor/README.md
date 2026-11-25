# Vendored crates

## ironrdp-connector 0.12.0

Copy of the crates.io release with one change, in `src/connection_activation.rs`:
the client General Capability Set also advertises `LONG_CREDENTIALS_SUPPORTED`.

Without that flag Windows Server 2012 completes the logon but never sends a single
graphics update (permanent black screen). Newer Windows versions do not care.
Wired in through `[patch.crates-io]` in `src-tauri/Cargo.toml`; drop this copy once
upstream IronRDP sets the flag itself.
