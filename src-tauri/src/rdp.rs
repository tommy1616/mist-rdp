use crate::ws_stream::{SessionStatusPayload, StreamHub};
use anyhow::{anyhow, Context, Result};
use futures_util::FutureExt;
use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy};
use ironrdp_connector::{BitmapConfig, ClientConnector, ConnectionOutcome, Credentials, DesktopSize};
use ironrdp_displaycontrol::client::DisplayControlClient;
use ironrdp_dvc::DrdynvcClient;
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_pdu::gcc::{ConnectionType, KeyboardType};
use ironrdp_pdu::input::fast_path::{FastPathInputEvent, KeyboardFlags, SynchronizeFlags};
use ironrdp_pdu::input::mouse::PointerFlags;
use ironrdp_pdu::input::MousePdu;
use ironrdp_pdu::rdp::capability_sets::{
    client_codecs_capabilities, BitmapCodecs, InputFlags, MajorPlatformType, RailSupportLevel,
};
use ironrdp_pdu::rdp::client_info::{CompressionType, PerformanceFlags, TimezoneInfo};
use ironrdp_rdpdr::backend::RdpdrBackendFactory;
use ironrdp_session::image::DecodedImage;
use ironrdp_session::{ActiveStageBuilder, ActiveStageOutput};
use ironrdp_tokio::reqwest::ReqwestNetworkClient;
use ironrdp_tokio::{
    connect_begin, connect_finalize, mark_as_upgraded, single_sequence_step_read,
    split_tokio_framed, FramedWrite, TokioFramed,
};
use serde::Deserialize;
use std::borrow::Cow;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::runtime::Handle as TokioHandle;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

#[cfg(windows)]
struct WinClipboardThreadGuard {
    thread_id: u32,
}

#[cfg(windows)]
impl Drop for WinClipboardThreadGuard {
    fn drop(&mut self) {
        if self.thread_id != 0 {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                    self.thread_id,
                    windows_sys::Win32::UI::WindowsAndMessaging::WM_QUIT,
                    0,
                    0,
                );
            }
        }
    }
}

/// Delegating CLIPRDR backend that records the channel handshake in the log and tells the
/// session loop when the server's `MonitorReady` arrived (the point at which the client must send
/// its Capabilities + initial Format List to complete channel initialization).
#[cfg(windows)]
#[derive(Debug)]
struct LoggingCliprdrBackend {
    inner: Box<dyn ironrdp_cliprdr::backend::CliprdrBackend>,
    gate: Arc<WinClipGate>,
}

#[cfg(windows)]
ironrdp_core::impl_as_any!(LoggingCliprdrBackend);

#[cfg(windows)]
impl ironrdp_cliprdr::backend::CliprdrBackend for LoggingCliprdrBackend {
    fn temporary_directory(&self) -> &str {
        self.inner.temporary_directory()
    }
    fn client_capabilities(&self) -> ironrdp_cliprdr::pdu::ClipboardGeneralCapabilityFlags {
        self.inner.client_capabilities()
    }
    fn on_ready(&mut self) {
        info!("CLIPRDR backend: channel initialized (on_ready)");
        self.inner.on_ready()
    }
    fn on_request_format_list(&mut self) {
        info!("CLIPRDR backend: server MonitorReady received; initial format list requested");
        self.gate.monitor_ready.store(true, Ordering::Release);
        self.inner.on_request_format_list()
    }
    fn on_format_list_response(&mut self, ok: bool) {
        info!(ok, "CLIPRDR backend: server answered our format list");
        self.inner.on_format_list_response(ok)
    }
    fn on_process_negotiated_capabilities(
        &mut self,
        capabilities: ironrdp_cliprdr::pdu::ClipboardGeneralCapabilityFlags,
    ) {
        info!(?capabilities, "CLIPRDR backend: negotiated capabilities");
        self.inner.on_process_negotiated_capabilities(capabilities)
    }
    fn on_remote_copy(&mut self, available_formats: &[ironrdp_cliprdr::pdu::ClipboardFormat]) {
        info!(count = available_formats.len(), "CLIPRDR backend: server copied (format list)");
        self.inner.on_remote_copy(available_formats)
    }
    fn on_format_data_request(&mut self, request: ironrdp_cliprdr::pdu::FormatDataRequest) {
        self.inner.on_format_data_request(request)
    }
    fn on_format_data_response(&mut self, response: ironrdp_cliprdr::pdu::FormatDataResponse<'_>) {
        self.inner.on_format_data_response(response)
    }
    fn on_file_contents_request(&mut self, request: ironrdp_cliprdr::pdu::FileContentsRequest) {
        self.inner.on_file_contents_request(request)
    }
    fn on_file_contents_response(&mut self, response: ironrdp_cliprdr::pdu::FileContentsResponse<'_>) {
        self.inner.on_file_contents_response(response)
    }
    fn on_lock(&mut self, data_id: ironrdp_cliprdr::pdu::LockDataId) {
        self.inner.on_lock(data_id)
    }
    fn on_unlock(&mut self, data_id: ironrdp_cliprdr::pdu::LockDataId) {
        self.inner.on_unlock(data_id)
    }
    fn on_remote_file_list(
        &mut self,
        files: &[ironrdp_cliprdr::pdu::FileDescriptor],
        clip_data_id: Option<u32>,
    ) {
        info!(count = files.len(), "CLIPRDR backend: server file list");
        self.inner.on_remote_file_list(files, clip_data_id)
    }
    fn on_outgoing_locks_cleared(&mut self, clip_data_ids: &[ironrdp_cliprdr::pdu::LockDataId]) {
        self.inner.on_outgoing_locks_cleared(clip_data_ids)
    }
    fn on_outgoing_locks_expired(&mut self, clip_data_ids: &[ironrdp_cliprdr::pdu::LockDataId]) {
        self.inner.on_outgoing_locks_expired(clip_data_ids)
    }
    fn now_ms(&self) -> u64 {
        self.inner.now_ms()
    }
    fn elapsed_ms(&self, since: u64) -> u64 {
        self.inner.elapsed_ms(since)
    }
}

const NO_GRAPHICS_MARK: &str = "[NO_GRAPHICS]";
/// Slow hosts can take well over ten seconds to paint; waiting too briefly tears down a session that was about to work.
const NO_GRAPHICS_WAIT_SECS: u64 = 45;

#[cfg(windows)]
const CLIP_SUBCLASS_ID: usize = 1;

/// Per-session guard installed on `ironrdp-cliprdr-native`'s hidden clipboard listener window.
///
/// Prevents three Windows CLIPRDR failure modes:
/// 1. Premature `SendInitiateCopy` while `Cliprdr` is still in `Initialization` (before the
///    server's `ServerCapabilities` + `MonitorReady` + `FormatListResponse::Ok` complete).
/// 2. Self-process echo / cross-tab COM theft when MistRDP itself writes to the OS clipboard
///    via `SetClipboardData` OR `OleSetClipboard` (`CLIPBRDWNDCLASS`, used for remote file copy).
/// 3. `OpenClipboard` contention between multiple open RDP tabs on the same host update.
#[cfg(windows)]
#[derive(Debug)]
struct WinClipGate {
    ready: AtomicBool,
    /// Set once the server's `MonitorReady` arrived. From then on a local copy is safe to send:
    /// `Cliprdr::initiate_copy` bundles Capabilities + Temporary Directory + Format List while the
    /// channel is still initializing. Waiting for `ready` instead deadlocks when the initial
    /// format list never went out (clipboard busy / empty), because `ready` itself needs that
    /// first format list to be answered.
    monitor_ready: AtomicBool,
    session_gen: u64,
    active_gen: Arc<AtomicU64>,
}

#[cfg(windows)]
impl WinClipGate {
    fn new(session_gen: u64, active_gen: Arc<AtomicU64>) -> Self {
        Self {
            ready: AtomicBool::new(false),
            monitor_ready: AtomicBool::new(false),
            session_gen,
            active_gen,
        }
    }

    fn should_forward_clipboard_update(&self, owner_is_self_process: bool) -> bool {
        if !(self.ready.load(Ordering::Acquire) || self.monitor_ready.load(Ordering::Acquire)) {
            return false;
        }
        if owner_is_self_process {
            return false;
        }
        let active = self.active_gen.load(Ordering::Acquire);
        active == 0 || active == self.session_gen
    }
}

#[cfg(windows)]
fn is_clipboard_owned_by_current_process() -> bool {
    use windows_sys::Win32::System::DataExchange::GetClipboardOwner;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

    unsafe {
        let owner = GetClipboardOwner();
        if owner.is_null() {
            return false;
        }
        let mut owner_pid: u32 = 0;
        GetWindowThreadProcessId(owner, &mut owner_pid);
        owner_pid != 0 && owner_pid == GetCurrentProcessId()
    }
}

#[cfg(windows)]
unsafe extern "system" fn clip_always_listen_subproc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
    id: usize,
    ref_data: usize,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        WM_ACTIVATE, WM_ACTIVATEAPP, WM_CLIPBOARDUPDATE, WM_NCDESTROY,
    };

    match msg {
        // Absorb activation notifications so ironrdp-cliprdr-native's inner subclass never flips
        // `window_is_active` back to `true` (which would drop host screenshots taken while active).
        WM_ACTIVATE | WM_ACTIVATEAPP => 0,
        WM_CLIPBOARDUPDATE => {
            if ref_data != 0 {
                let gate = unsafe { &*(ref_data as *const WinClipGate) };
                if !gate.should_forward_clipboard_update(is_clipboard_owned_by_current_process()) {
                    return 0;
                }
            }
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        WM_NCDESTROY => {
            let removed =
                unsafe { RemoveWindowSubclass(hwnd, Some(clip_always_listen_subproc), id) } != 0;
            if removed && ref_data != 0 {
                unsafe { drop(Arc::from_raw(ref_data as *const WinClipGate)) };
            }
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(windows)]
unsafe extern "system" fn find_clip_monitor_enum_proc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::BOOL {
    let mut buf = [0u8; 64];
    let len = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameA(
            hwnd,
            buf.as_mut_ptr(),
            buf.len() as i32,
        )
    };
    if len > 0 && &buf[..len as usize] == b"IronRDPClipboardMonitor" {
        let out = lparam as *mut windows_sys::Win32::Foundation::HWND;
        if !out.is_null() {
            unsafe { *out = hwnd };
        }
        return 0;
    }
    1
}

#[cfg(windows)]
fn patch_win_clipboard_always_listen(
    tid: u32,
    gate: Arc<WinClipGate>,
) -> Option<(windows_sys::Win32::Foundation::HWND, *const WinClipGate)> {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::Shell::SetWindowSubclass;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumThreadWindows, SendMessageW, WM_ACTIVATEAPP,
    };

    let mut hwnd: HWND = std::ptr::null_mut();
    unsafe {
        EnumThreadWindows(
            tid,
            Some(find_clip_monitor_enum_proc),
            (&mut hwnd as *mut HWND) as LPARAM,
        );
    }
    if hwnd.is_null() {
        return None;
    }

    // 1) Flip ironrdp-cliprdr-native's initial `window_is_active = true` -> `false`.
    unsafe {
        SendMessageW(hwnd, WM_ACTIVATEAPP, 0, 0);
    }

    // 2) Install outer subclass with our WinClipGate guard.
    let raw_gate = Arc::into_raw(gate);
    let ok = unsafe {
        SetWindowSubclass(
            hwnd,
            Some(clip_always_listen_subproc),
            CLIP_SUBCLASS_ID,
            raw_gate as usize,
        )
    } != 0;
    if ok {
        Some((hwnd, raw_gate))
    } else {
        unsafe { drop(Arc::from_raw(raw_gate)) };
        None
    }
}

#[cfg(windows)]
fn unpatch_win_clipboard_always_listen(
    hwnd: windows_sys::Win32::Foundation::HWND,
    raw_gate: *const WinClipGate,
) {
    use windows_sys::Win32::UI::Shell::RemoveWindowSubclass;
    if !hwnd.is_null() {
        let removed = unsafe {
            RemoveWindowSubclass(hwnd, Some(clip_always_listen_subproc), CLIP_SUBCLASS_ID)
        } != 0;
        if removed && !raw_gate.is_null() {
            unsafe { drop(Arc::from_raw(raw_gate)) };
        }
    }
}

#[cfg(windows)]
fn spawn_win_clipboard(
    proxy: SessionClipboardProxy,
    gate: Arc<WinClipGate>,
) -> Result<(Box<dyn ironrdp_cliprdr::backend::CliprdrBackend>, WinClipboardThreadGuard)> {
    let (init_tx, init_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("mistrdp-win-cliprdr".to_owned())
        .spawn(move || {
            let com_hr = unsafe {
                windows_sys::Win32::System::Com::CoInitializeEx(
                    std::ptr::null(),
                    windows_sys::Win32::System::Com::COINIT_APARTMENTTHREADED as u32,
                )
            };
            let com_inited = com_hr >= 0;
            if !com_inited {
                warn!(hr = format_args!("0x{:08X}", com_hr as u32), "CoInitializeEx(STA) failed on clipboard thread");
            }

            let tid = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
            match ironrdp_cliprdr_native::WinClipboard::with_file_transfer(proxy, true) {
                Ok(win_cb) => {
                    let gate_for_backend = gate.clone();
                    let patched = patch_win_clipboard_always_listen(tid, gate);
                    info!(
                        com_sta = com_inited,
                        always_listen = patched.is_some(),
                        "WinClipboard initialized with file transfer enabled"
                    );
                    let backend: Box<dyn ironrdp_cliprdr::backend::CliprdrBackend> =
                        Box::new(LoggingCliprdrBackend {
                            inner: win_cb.backend_factory().build_cliprdr_backend(),
                            gate: gate_for_backend,
                        });
                    let _ = init_tx.send(Ok((backend, tid)));

                    unsafe {
                        let mut msg: windows_sys::Win32::UI::WindowsAndMessaging::MSG =
                            std::mem::zeroed();
                        while windows_sys::Win32::UI::WindowsAndMessaging::GetMessageW(
                            &mut msg,
                            std::ptr::null_mut(),
                            0,
                            0,
                        ) > 0
                        {
                            windows_sys::Win32::UI::WindowsAndMessaging::TranslateMessage(&msg);
                            windows_sys::Win32::UI::WindowsAndMessaging::DispatchMessageW(&msg);
                        }
                    }
                    if let Some((hwnd, raw_gate)) = patched {
                        unpatch_win_clipboard_always_listen(hwnd, raw_gate);
                    }
                    drop(win_cb);
                }
                Err(e) => {
                    let _ = init_tx.send(Err(anyhow!("WinClipboard init failed: {e}")));
                }
            }

            if com_inited {
                unsafe { windows_sys::Win32::System::Com::CoUninitialize() };
            }
        })
        .context("Windows clipboard thread start failed")?;

    let (backend, thread_id) = init_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| anyhow!("Windows clipboard init timed out"))??;

    Ok((backend, WinClipboardThreadGuard { thread_id }))
}

/// Performance flags matching native Windows mstsc on LAN: keep full-window dragging
/// (live window contents instead of wireframe outline), ClearType font smoothing, and DWM composition.
pub fn default_performance_flags() -> PerformanceFlags {
    PerformanceFlags::ENABLE_FONT_SMOOTHING | PerformanceFlags::ENABLE_DESKTOP_COMPOSITION
}

/// Build RDP bitmap capability configuration for the chosen quality mode.
/// - `lossless = true`: `None` (lossless 32-bit RGB raw/RLE bitmaps for crisp ClearType text)
/// - `lossless = false`: RemoteFX + lossy compression for lower bandwidth usage
pub fn bitmap_config_for_quality(lossless: bool) -> Option<BitmapConfig> {
    if lossless {
        None
    } else {
        let codecs = client_codecs_capabilities(&[]).unwrap_or_else(|_| BitmapCodecs(Vec::new()));
        Some(BitmapConfig {
            lossy_compression: true,
            color_depth: 32,
            codecs,
        })
    }
}

/// Wrapper around CPAL RDPSND backend allowing instant live mute/unmute without reconnecting.
#[derive(Debug)]
struct MutableRdpsndBackend {
    inner: ironrdp_rdpsnd_native::cpal::RdpsndBackend,
    enabled: Arc<AtomicBool>,
    was_enabled: bool,
}

impl MutableRdpsndBackend {
    fn new(enabled: Arc<AtomicBool>) -> Self {
        Self {
            inner: ironrdp_rdpsnd_native::cpal::RdpsndBackend::new(),
            enabled,
            was_enabled: false,
        }
    }
}

impl ironrdp_rdpsnd::client::RdpsndClientHandler for MutableRdpsndBackend {
    fn get_flags(&self) -> ironrdp_rdpsnd::pdu::AudioFormatFlags {
        self.inner.get_flags()
    }

    fn get_formats(&self) -> &[ironrdp_rdpsnd::pdu::AudioFormat] {
        self.inner.get_formats()
    }

    fn choose_formats(
        &self,
        server_formats: &[ironrdp_rdpsnd::pdu::AudioFormat],
    ) -> Vec<ironrdp_rdpsnd::pdu::AudioFormat> {
        self.inner.choose_formats(server_formats)
    }

    fn wave(
        &mut self,
        format: &ironrdp_rdpsnd::pdu::AudioFormat,
        ts: u32,
        data: Cow<'_, [u8]>,
    ) {
        if !self.enabled.load(Ordering::Relaxed) {
            if self.was_enabled {
                self.inner.close();
                self.was_enabled = false;
            }
            return;
        }
        self.was_enabled = true;
        self.inner.wave(format, ts, data);
    }

    fn playback_latency(&self) -> Duration {
        if self.enabled.load(Ordering::Relaxed) {
            self.inner.playback_latency()
        } else {
            Duration::ZERO
        }
    }

    fn set_volume(&mut self, volume: ironrdp_rdpsnd::pdu::VolumePdu) {
        self.inner.set_volume(volume);
    }

    fn set_pitch(&mut self, pitch: ironrdp_rdpsnd::pdu::PitchPdu) {
        self.inner.set_pitch(pitch);
    }

    fn close(&mut self) {
        self.inner.close();
        self.was_enabled = false;
    }
}

/// Per MS-RDPBCGR 2.2.8.1.2, Fast-Path input events may only be sent when the server
/// advertised `FASTPATH_INPUT` or `FASTPATH_INPUT_2` in its Input Capability Set.
pub fn supports_fastpath_input(flags: InputFlags) -> bool {
    flags.intersects(InputFlags::FASTPATH_INPUT | InputFlags::FASTPATH_INPUT_2)
}

/// Build RDP Fast-Path lock-key synchronize flags from local Caps/Num/Scroll Lock states.
pub fn build_sync_flags(scroll: bool, num: bool, caps: bool) -> SynchronizeFlags {
    let mut flags = SynchronizeFlags::empty();
    if scroll {
        flags |= SynchronizeFlags::SCROLL_LOCK;
    }
    if num {
        flags |= SynchronizeFlags::NUM_LOCK;
    }
    if caps {
        flags |= SynchronizeFlags::CAPS_LOCK;
    }
    flags
}

/// Derive a safe ASCII RDPDR share name from a folder path.
pub fn sanitize_drive_name(path: &Path) -> String {
    let raw = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("MistShare");
    let clean: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(24)
        .collect();
    if clean.is_empty() {
        "MistShare".to_string()
    } else {
        clean
    }
}

/// Normalize a user-provided folder path and verify it is an existing directory.
/// Strips Windows verbatim `\\?\` prefix because `RedirectedDrive::new` requires a normal drive-letter path.
pub fn normalize_share_folder(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = PathBuf::from(trimmed);
    let canon = std::fs::canonicalize(&path).unwrap_or(path);
    let cleaned = {
        let s = canon.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            if rest.starts_with("UNC\\") {
                return None;
            }
            PathBuf::from(rest)
        } else {
            canon
        }
    };
    if cleaned.is_dir() {
        Some(cleaned)
    } else {
        None
    }
}

#[cfg(windows)]
fn build_rdpdr_channel(folder: &Path) -> Result<ironrdp_rdpdr::Rdpdr> {
    let drive_name = sanitize_drive_name(folder);
    let drive = ironrdp_rdpdr_native::RedirectedDrive::new(1, drive_name, folder, false)
        .map_err(|e| anyhow!("無效的共享資料夾：{e:?}"))?;
    let product = ironrdp_rdpdr_native::WindowsRdpdrBackendFactory::new(drive)
        .build_rdpdr_backend()
        .map_err(|e| anyhow!("初始化資料夾共享失敗：{e}"))?;
    let (backend, drives) = product.into_parts();
    let initial_drives: Vec<(u32, String)> = drives.into_iter().map(|d| d.into_parts()).collect();
    Ok(ironrdp_rdpdr::Rdpdr::new(backend, "MistRDP".to_owned()).with_drives(Some(initial_drives)))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn build_rdpdr_channel(folder: &Path) -> Result<ironrdp_rdpdr::Rdpdr> {
    let drive_name = sanitize_drive_name(folder);
    let folder_str = folder
        .to_str()
        .ok_or_else(|| anyhow!("共享資料夾路徑包含不支援的字元"))?
        .to_owned();
    let product = ironrdp_rdpdr_native::backend::NixRdpdrBackendFactory::new(folder_str)
        .with_drive(1, drive_name)
        .build_rdpdr_backend()
        .map_err(|e| anyhow!("初始化資料夾共享失敗：{e}"))?;
    let (backend, drives) = product.into_parts();
    let initial_drives: Vec<(u32, String)> = drives.into_iter().map(|d| d.into_parts()).collect();
    Ok(ironrdp_rdpdr::Rdpdr::new(backend, "MistRDP".to_owned()).with_drives(Some(initial_drives)))
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConnectParams {
    pub session_id: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub domain: Option<String>,
    pub password: String,
    pub width: u16,
    pub height: u16,
    pub clipboard_enabled: bool,
    pub audio_enabled: bool,
    pub lossless_quality: bool,
    pub shared_folder: Option<String>,
    pub accept_self_signed_tls: bool,
}

#[derive(Debug)]
pub enum SessionInput {
    FastPath(Vec<FastPathInputEvent>),
    Clipboard(ClipboardMessage),
    Resize { width: u16, height: u16 },
    Disconnect,
}

#[derive(Clone)]
struct SessionClipboardProxy {
    tx: mpsc::UnboundedSender<SessionInput>,
}

impl core::fmt::Debug for SessionClipboardProxy {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SessionClipboardProxy").finish()
    }
}

impl ClipboardMessageProxy for SessionClipboardProxy {
    fn send_clipboard_message(&self, message: ClipboardMessage) {
        let _ = self.tx.send(SessionInput::Clipboard(message));
    }
}

static NEXT_SESSION_GEN: AtomicU64 = AtomicU64::new(1);

struct SessionHandle {
    generation: u64,
    tx: mpsc::UnboundedSender<SessionInput>,
    audio_enabled: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct RdpManager {
    sessions: Arc<Mutex<HashMap<String, SessionHandle>>>,
    active_clip_gen: Arc<AtomicU64>,
    hub: StreamHub,
    rt: TokioHandle,
}

/// Minimal bounds-checked ASN.1 DER TLV reader: returns `(tag, value_slice, remainder)`.
fn read_asn1_tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    if input.len() < 2 {
        return None;
    }
    let tag = input[0];
    let len0 = input[1];
    let (len, header_len) = if len0 < 0x80 {
        (usize::from(len0), 2usize)
    } else {
        let num_bytes = usize::from(len0 & 0x7F);
        if num_bytes == 0 || num_bytes > 4 || input.len() < 2 + num_bytes {
            return None;
        }
        let mut len = 0usize;
        for &b in &input[2..2 + num_bytes] {
            len = (len << 8) | usize::from(b);
        }
        (len, 2 + num_bytes)
    };
    let end = header_len.checked_add(len)?;
    if end > input.len() {
        return None;
    }
    Some((tag, &input[header_len..end], &input[end..]))
}

/// Extract the raw `subjectPublicKey` bit-string bytes (without leading unused-bits octet)
/// from an X.509 certificate DER blob for CredSSP `pubKeyAuth`.
///
/// Unlike strict `x509-cert` DER parsers, this skips non-key fields (`issuer`, `validity`,
/// `subject`) by TLV length so LAN Windows Server self-signed RDP certificates containing
/// non-canonical encodings (e.g. `_` inside `PrintableString` CN) succeed reliably.
pub fn extract_spki_public_key_from_cert_der(der: &[u8]) -> Option<Vec<u8>> {
    // Certificate ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signatureValue }
    let (cert_tag, cert_body, _) = read_asn1_tlv(der)?;
    if cert_tag != 0x30 {
        return None;
    }
    // TBSCertificate ::= SEQUENCE { [0] version OPTIONAL, serial, sig, issuer, validity, subject, spki, ... }
    let (tbs_tag, mut tbs, _) = read_asn1_tlv(cert_body)?;
    if tbs_tag != 0x30 {
        return None;
    }
    if tbs.first() == Some(&0xA0) {
        let (_, _, rest) = read_asn1_tlv(tbs)?;
        tbs = rest;
    }
    // Skip serialNumber, signature, issuer, validity, subject (5 fields)
    for _ in 0..5 {
        let (_, _, rest) = read_asn1_tlv(tbs)?;
        tbs = rest;
    }
    // SubjectPublicKeyInfo ::= SEQUENCE { algorithm, subjectPublicKey BIT STRING }
    let (spki_tag, spki_body, _) = read_asn1_tlv(tbs)?;
    if spki_tag != 0x30 {
        return None;
    }
    let (_, _, spki_rest) = read_asn1_tlv(spki_body)?;
    let (bs_tag, bs_val, _) = read_asn1_tlv(spki_rest)?;
    if bs_tag != 0x03 || bs_val.len() < 2 || bs_val[0] != 0 {
        return None;
    }
    Some(bs_val[1..].to_vec())
}

/// Upgrade an RDP TCP stream using the OS-native TLS engine (Windows Schannel / macOS SecureTransport)
/// so LAN Windows Servers with self-signed certs, static RSA cipher suites, non-DNS NetBIOS names,
/// or legacy TLS versions connect just like native `mstsc.exe`.
async fn upgrade_rdp_tls(
    stream: TcpStream,
    host: &str,
    accept_self_signed: bool,
) -> Result<(tokio_native_tls::TlsStream<TcpStream>, Vec<u8>)> {
    let mut builder = tokio_native_tls::native_tls::TlsConnector::builder();
    if accept_self_signed {
        builder.danger_accept_invalid_certs(true);
        builder.danger_accept_invalid_hostnames(true);
        builder.use_sni(false);
        builder.min_protocol_version(Some(tokio_native_tls::native_tls::Protocol::Tlsv10));
    }
    let native_connector = builder
        .build()
        .map_err(|e| anyhow!("TLS 初始化失敗：{e}"))?;
    let connector = tokio_native_tls::TlsConnector::from(native_connector);

    let tls_stream = connector
        .connect(host, stream)
        .await
        .map_err(|e| anyhow!("TLS 連線失敗：{e}"))?;

    let peer_cert = tls_stream
        .get_ref()
        .peer_certificate()
        .map_err(|e| anyhow!("讀取伺服器憑證失敗：{e}"))?
        .ok_or_else(|| anyhow!("伺服器未提供 TLS 憑證"))?;

    let cert_der = peer_cert
        .to_der()
        .map_err(|e| anyhow!("匯出伺服器憑證 DER 失敗：{e}"))?;

    let server_public_key = extract_spki_public_key_from_cert_der(&cert_der)
        .ok_or_else(|| anyhow!("無法從伺服器 TLS 憑證解析公鑰"))?;

    Ok((tls_stream, server_public_key))
}

/// Classify IronRDP / CredSSP / network errors into clear user-facing codes & messages.
pub fn classify_error_text(raw: &str) -> (&'static str, String) {
    let lower = raw.to_lowercase();

    if lower.contains("account_locked_out") || lower.contains("0xc0000234") {
        return ("AUTH_FAILED", "帳號已被鎖定".to_string());
    }
    if lower.contains("password_expired")
        || lower.contains("password_must_change")
        || lower.contains("0xc0000071")
        || lower.contains("0xc0000224")
    {
        return ("AUTH_FAILED", "密碼已過期或需要變更".to_string());
    }
    if lower.contains("account_disabled") || lower.contains("0xc0000072") {
        return ("AUTH_FAILED", "帳號已被停用".to_string());
    }
    if lower.contains("logon_failure")
        || lower.contains("logonfailure")
        || lower.contains("logon_denied")
        || lower.contains("0xc000006d")
        || lower.contains("0xc000006a")
        || lower.contains("0x8009030c")
        || lower.contains("wrong_password")
        || lower.contains("wrongpassword")
        || lower.contains("invalid_credentials")
        || lower.contains("invalidcredentials")
        || lower.contains("bad_password")
        || lower.contains("access_denied")
        || lower.contains("accessdenied")
        || lower.contains("0xc0000022")
        || lower.contains("credssp")
        || lower.contains("ntlm")
        || lower.contains("errinfo_logon")
        || lower.contains("authentication")
        || lower.contains("[auth_failed]")
    {
        return ("AUTH_FAILED", "帳號或密碼錯誤".to_string());
    }
    if lower.contains("連線逾時") || lower.contains("timed out") {
        return ("TIMEOUT", "連線逾時".to_string());
    }
    if lower.contains("tls") || lower.contains("certificate") || lower.contains("憑證") {
        let short = raw.split(" | ").next().unwrap_or(raw).trim();
        let detail: String = short.chars().take(180).collect();
        let msg = if detail.is_empty() {
            "TLS 連線失敗".to_string()
        } else if detail.starts_with("TLS") {
            detail
        } else {
            format!("TLS 連線失敗：{detail}")
        };
        return ("TLS_ERROR", msg);
    }
    if lower.contains("tcp 連線失敗")
        || lower.contains("connection refused")
        || lower.contains("unreachable")
        || lower.contains("dns")
    {
        return ("NETWORK_ERROR", format!("無法連線至主機：{raw}"));
    }

    ("ERROR", raw.to_string())
}

impl RdpManager {
    pub fn new(hub: StreamHub, rt: TokioHandle) -> Self {
        let manager = Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            active_clip_gen: Arc::new(AtomicU64::new(0)),
            hub,
            rt,
        };
        let input = manager.clone();
        manager
            .hub
            .set_input_sink(Arc::new(move |frame| input.handle_ws_input(frame)));
        manager
    }

    /// Keyboard frames from the WebView arrive over the ordered loopback socket:
    /// `0x10 | sid_len u16 | sid | scancode | flags(1=extended,2=pressed)` and
    /// `0x11 | sid_len u16 | sid | code_unit u16 | pressed` and
    /// `0x12 | sid_len u16 | sid | x u16 | y u16 | button | action(0 move,1 down,2 up,3 wheel,4 hwheel) | delta i16`.
    fn handle_ws_input(&self, frame: &[u8]) {
        let (Some(&kind), Some(len)) = (frame.first(), frame.get(1..3)) else { return };
        let sid_end = 3 + u16::from_le_bytes([len[0], len[1]]) as usize;
        let (Some(sid), Some(body)) = (
            frame.get(3..sid_end).and_then(|b| std::str::from_utf8(b).ok()),
            frame.get(sid_end..),
        ) else { return };
        match (kind, body) {
            (0x10, [sc, flags]) => self.send_key(sid, *sc, flags & 1 != 0, flags & 2 != 0),
            (0x11, [lo, hi, pressed]) => {
                self.send_unicode(sid, u16::from_le_bytes([*lo, *hi]), *pressed != 0)
            }
            (0x12, [x0, x1, y0, y1, button, action, d0, d1]) => {
                let action = match action {
                    0 => "move",
                    1 => "down",
                    2 => "up",
                    3 => "wheel",
                    4 => "hwheel",
                    _ => return,
                };
                self.send_mouse(
                    sid,
                    u16::from_le_bytes([*x0, *x1]),
                    u16::from_le_bytes([*y0, *y1]),
                    *button,
                    action,
                    i16::from_le_bytes([*d0, *d1]),
                );
            }
            _ => {}
        }
    }

    pub fn set_active_session(&self, session_id: Option<&str>) {
        let Some(sid) = session_id else {
            return;
        };
        if let Ok(map) = self.sessions.lock() {
            if let Some(h) = map.get(sid) {
                self.active_clip_gen.store(h.generation, Ordering::Release);
            }
        }
    }

    pub fn connect(&self, params: ConnectParams) -> Result<()> {
        let sid = params.session_id.clone();
        self.disconnect(&sid);

        let generation = NEXT_SESSION_GEN.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        let audio_enabled = Arc::new(AtomicBool::new(params.audio_enabled));
        {
            let mut map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            map.insert(
                sid.clone(),
                SessionHandle {
                    generation,
                    tx: tx.clone(),
                    audio_enabled: audio_enabled.clone(),
                },
            );
        }

        self.active_clip_gen.store(generation, Ordering::Release);

        let hub = self.hub.clone();
        let sessions = self.sessions.clone();
        let active_clip_gen = self.active_clip_gen.clone();

        self.rt.spawn(async move {
            let mut rx = rx;
            let mut params = params;
            let mut outcome = AssertUnwindSafe(run_session(
                params.clone(),
                generation,
                active_clip_gen.clone(),
                tx.clone(),
                &mut rx,
                hub.clone(),
                audio_enabled.clone(),
            ))
            .catch_unwind()
            .await;

            // RemoteFX mode produced a black screen: retry once in classic lossless mode.
            let black_screen = matches!(&outcome, Ok(Err(e)) if format!("{e:#}").contains(NO_GRAPHICS_MARK));
            if black_screen && !params.lossless_quality {
                info!(session_id = %sid, "Retrying session in classic lossless mode after black screen");
                params.lossless_quality = true;
                outcome = AssertUnwindSafe(run_session(
                    params.clone(),
                    generation,
                    active_clip_gen.clone(),
                    tx.clone(),
                    &mut rx,
                    hub.clone(),
                    audio_enabled,
                ))
                .catch_unwind()
                .await;
            }

            match outcome {
                Ok(Ok(())) => {
                    hub.send_status(&SessionStatusPayload {
                        session_id: &sid,
                        state: "disconnected",
                        code: None,
                        message: "已中斷連線",
                        width: params.width,
                        height: params.height,
                        rtt_ms: None,
                    });
                }
                Ok(Err(err)) => {
                    let full_detail = format!("{err:?} | {err:#}");
                    error!(session_id = %sid, detail = %full_detail, "RDP session ended with error");
                    let (code, friendly) = classify_error_text(&full_detail);
                    hub.send_status(&SessionStatusPayload {
                        session_id: &sid,
                        state: "error",
                        code: Some(code),
                        message: &friendly,
                        width: params.width,
                        height: params.height,
                        rtt_ms: None,
                    });
                }
                Err(panic_payload) => {
                    let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                        (*s).to_string()
                    } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "內部錯誤".to_string()
                    };
                    error!(session_id = %sid, panic = %msg, "RDP session task panicked");
                    hub.send_status(&SessionStatusPayload {
                        session_id: &sid,
                        state: "error",
                        code: Some("INTERNAL"),
                        message: &format!("內部錯誤：{msg}"),
                        width: params.width,
                        height: params.height,
                        rtt_ms: None,
                    });
                }
            }

            if let Ok(mut map) = sessions.lock() {
                if map.get(&sid).is_some_and(|h| h.generation == generation) {
                    map.remove(&sid);
                    if active_clip_gen.load(Ordering::Acquire) == generation {
                        let fallback = map.values().next().map_or(0, |h| h.generation);
                        active_clip_gen.store(fallback, Ordering::Release);
                    }
                }
            }
        });

        Ok(())
    }

    pub fn disconnect(&self, session_id: &str) {
        if let Ok(mut map) = self.sessions.lock() {
            if let Some(handle) = map.remove(session_id) {
                let _ = handle.tx.send(SessionInput::Disconnect);
                if self.active_clip_gen.load(Ordering::Acquire) == handle.generation {
                    let fallback = map.values().next().map_or(0, |h| h.generation);
                    self.active_clip_gen.store(fallback, Ordering::Release);
                }
            }
        }
    }

    pub fn set_audio_enabled(&self, session_id: &str, enabled: bool) {
        if let Ok(map) = self.sessions.lock() {
            if let Some(handle) = map.get(session_id) {
                handle.audio_enabled.store(enabled, Ordering::Relaxed);
            }
        }
    }

    pub fn send_key(&self, session_id: &str, scancode: u8, extended: bool, pressed: bool) {
        let mut flags = KeyboardFlags::empty();
        if !pressed {
            flags |= KeyboardFlags::RELEASE;
        }
        if extended {
            flags |= KeyboardFlags::EXTENDED;
        }
        self.send_fastpath(
            session_id,
            vec![FastPathInputEvent::KeyboardEvent(flags, scancode)],
        );
    }

    pub fn send_unicode(&self, session_id: &str, code_unit: u16, pressed: bool) {
        let flags = if pressed {
            KeyboardFlags::empty()
        } else {
            KeyboardFlags::RELEASE
        };
        self.send_fastpath(
            session_id,
            vec![FastPathInputEvent::UnicodeKeyboardEvent(flags, code_unit)],
        );
    }

    pub fn send_mouse(
        &self,
        session_id: &str,
        x: u16,
        y: u16,
        button: u8,
        action: &str,
        wheel_delta: i16,
    ) {
        let mut flags = PointerFlags::empty();
        let mut wheel_units: i16 = 0;

        match action {
            "move" => flags |= PointerFlags::MOVE,
            "down" => {
                flags |= PointerFlags::DOWN;
                match button {
                    0 => flags |= PointerFlags::LEFT_BUTTON,
                    1 => flags |= PointerFlags::MIDDLE_BUTTON_OR_WHEEL,
                    2 => flags |= PointerFlags::RIGHT_BUTTON,
                    _ => flags |= PointerFlags::LEFT_BUTTON,
                }
            }
            "up" => match button {
                0 => flags |= PointerFlags::LEFT_BUTTON,
                1 => flags |= PointerFlags::MIDDLE_BUTTON_OR_WHEEL,
                2 => flags |= PointerFlags::RIGHT_BUTTON,
                _ => flags |= PointerFlags::LEFT_BUTTON,
            },
            "wheel" => {
                flags |= PointerFlags::VERTICAL_WHEEL;
                if wheel_delta < 0 {
                    flags |= PointerFlags::WHEEL_NEGATIVE;
                }
                wheel_units = wheel_delta;
            }
            "hwheel" => {
                flags |= PointerFlags::HORIZONTAL_WHEEL;
                if wheel_delta < 0 {
                    flags |= PointerFlags::WHEEL_NEGATIVE;
                }
                wheel_units = wheel_delta;
            }
            _ => return,
        }

        let pdu = MousePdu {
            flags,
            number_of_wheel_rotation_units: wheel_units,
            x_position: x,
            y_position: y,
        };
        self.send_fastpath(session_id, vec![FastPathInputEvent::MouseEvent(pdu)]);
    }

    pub fn send_ctrl_alt_del(&self, session_id: &str) {
        let events = vec![
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x1D),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x38),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::EXTENDED, 0x53),
            FastPathInputEvent::KeyboardEvent(
                KeyboardFlags::EXTENDED | KeyboardFlags::RELEASE,
                0x53,
            ),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x38),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x1D),
        ];
        self.send_fastpath(session_id, events);
    }

    /// Synchronize remote CapsLock / NumLock / ScrollLock LED & input states with local host.
    pub fn sync_locks(&self, session_id: &str, scroll: bool, num: bool, caps: bool) {
        let flags = build_sync_flags(scroll, num, caps);
        self.send_fastpath(session_id, vec![FastPathInputEvent::SyncEvent(flags)]);
    }

    /// Send Pause/Break key using the canonical MS-RDPBCGR EXTENDED1 0x1D + 0x45 sequence.
    pub fn send_pause(&self, session_id: &str) {
        let events = vec![
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::EXTENDED1, 0x1D),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x45),
            FastPathInputEvent::KeyboardEvent(
                KeyboardFlags::EXTENDED1 | KeyboardFlags::RELEASE,
                0x1D,
            ),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x45),
        ];
        self.send_fastpath(session_id, events);
    }

    /// Native mstsc Ctrl+Alt+Numpad- (active window) / Ctrl+Alt+Numpad+ (full desktop) screenshot.
    pub fn send_remote_screenshot(&self, session_id: &str, active_window_only: bool) {
        let mut events = vec![
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x1D),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::EXTENDED | KeyboardFlags::RELEASE, 0x1D),
        ];
        if active_window_only {
            events.push(FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x38));
            events.push(FastPathInputEvent::KeyboardEvent(KeyboardFlags::EXTENDED, 0x37));
            events.push(FastPathInputEvent::KeyboardEvent(
                KeyboardFlags::EXTENDED | KeyboardFlags::RELEASE,
                0x37,
            ));
            events.push(FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x38));
        } else {
            events.push(FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x38));
            events.push(FastPathInputEvent::KeyboardEvent(
                KeyboardFlags::EXTENDED | KeyboardFlags::RELEASE,
                0x38,
            ));
            events.push(FastPathInputEvent::KeyboardEvent(KeyboardFlags::EXTENDED, 0x37));
            events.push(FastPathInputEvent::KeyboardEvent(
                KeyboardFlags::EXTENDED | KeyboardFlags::RELEASE,
                0x37,
            ));
        }
        self.send_fastpath(session_id, events);
    }

    /// Native mstsc Alt+Home -> remote Start menu (Ctrl+Esc).
    pub fn send_remote_start_menu(&self, session_id: &str) {
        let events = vec![
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x38),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::EXTENDED | KeyboardFlags::RELEASE, 0x38),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x1D),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x01),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x01),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x1D),
        ];
        self.send_fastpath(session_id, events);
    }

    /// Native mstsc Alt+Delete -> remote active window system menu (Alt+Space).
    pub fn send_remote_window_menu(&self, session_id: &str) {
        let events = vec![
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x38),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x39),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x39),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x38),
        ];
        self.send_fastpath(session_id, events);
    }

    /// Native mstsc Alt+Insert -> cycle remote windows in start order (Alt+Esc).
    pub fn send_remote_cycle_order(&self, session_id: &str) {
        let events = vec![
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x38),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x01),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x01),
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x38),
        ];
        self.send_fastpath(session_id, events);
    }

    /// Native mstsc Alt+PageUp / Alt+PageDown -> step remote Alt+Tab switcher while Alt stays held.
    pub fn send_remote_switch_app(&self, session_id: &str, reverse: bool) {
        let events = if reverse {
            vec![
                FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x2A),
                FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x0F),
                FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x0F),
                FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x2A),
            ]
        } else {
            vec![
                FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x0F),
                FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x0F),
            ]
        };
        self.send_fastpath(session_id, events);
    }

    /// Release all modifier keys (Ctrl / Alt / Shift / Win) to prevent stuck keys on focus loss.
    pub fn release_all_modifiers(&self, session_id: &str) {
        let events = vec![
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x1D), // LCtrl
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::EXTENDED | KeyboardFlags::RELEASE, 0x1D), // RCtrl
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x38), // LAlt
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::EXTENDED | KeyboardFlags::RELEASE, 0x38), // RAlt
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x2A), // LShift
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x36), // RShift
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::EXTENDED | KeyboardFlags::RELEASE, 0x5B), // LWin
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::EXTENDED | KeyboardFlags::RELEASE, 0x5C), // RWin
        ];
        self.send_fastpath(session_id, events);
    }

    pub fn resize(&self, session_id: &str, width: u16, height: u16) {
        if let Ok(map) = self.sessions.lock() {
            if let Some(h) = map.get(session_id) {
                let _ = h.tx.send(SessionInput::Resize { width, height });
            }
        }
    }

    fn send_fastpath(&self, session_id: &str, events: Vec<FastPathInputEvent>) {
        if let Ok(map) = self.sessions.lock() {
            if let Some(h) = map.get(session_id) {
                let _ = h.tx.send(SessionInput::FastPath(events));
            }
        }
    }
}

async fn run_session(
    params: ConnectParams,
    session_gen: u64,
    active_clip_gen: Arc<AtomicU64>,
    input_tx: mpsc::UnboundedSender<SessionInput>,
    input_rx: &mut mpsc::UnboundedReceiver<SessionInput>,
    hub: StreamHub,
    audio_enabled: Arc<AtomicBool>,
) -> Result<()> {
    #[cfg(not(windows))]
    let _ = (session_gen, &active_clip_gen);
    let sid = params.session_id.clone();
    let host = params.host.trim().to_string();
    let width = params.width.clamp(640, 4096) & !1;
    let height = params.height.clamp(480, 2160) & !1;

    hub.send_status(&SessionStatusPayload {
        session_id: &sid,
        state: "connecting",
        code: None,
        message: &format!("{}:{}", host, params.port),
        width,
        height,
        rtt_ms: None,
    });

    let t0 = Instant::now();
    let addr_str = format!("{}:{}", host, params.port);
    let tcp_stream = tokio::time::timeout(Duration::from_secs(12), TcpStream::connect(&addr_str))
        .await
        .map_err(|_| anyhow!("連線逾時：{addr_str}"))?
        .with_context(|| format!("TCP 連線失敗：{addr_str}"))?;

    let _ = tcp_stream.set_nodelay(true);
    let rtt_ms = t0.elapsed().as_millis().min(u32::MAX as u128) as u32;
    let client_addr = tcp_stream.local_addr().context("取得本機位址失敗")?;

    let config = ironrdp_connector::Config {
        credentials: Credentials::UsernamePassword {
            username: params.username.clone(),
            password: params.password.clone(),
        },
        domain: params.domain.clone().filter(|d| !d.trim().is_empty()),
        enable_tls: true,
        enable_credssp: true,
        enable_standard_rdp_security: false,
        keyboard_type: KeyboardType::IBM_ENHANCED,
        keyboard_subtype: 0,
        keyboard_layout: 0,
        keyboard_functional_keys_count: 12,
        connection_type: ConnectionType::Lan,
        ime_file_name: String::new(),
        dig_product_id: String::new(),
        desktop_size: DesktopSize { width, height },
        monitor_layout: None,
        bitmap: bitmap_config_for_quality(params.lossless_quality),
        client_build: 19041,
        client_name: "MistRDP".to_owned(),
        client_dir: "C:\\Windows\\System32\\mstscax.dll".to_owned(),
        #[cfg(windows)]
        platform: MajorPlatformType::WINDOWS,
        #[cfg(target_os = "macos")]
        platform: MajorPlatformType::MACINTOSH,
        #[cfg(not(any(windows, target_os = "macos")))]
        platform: MajorPlatformType::UNIX,
        enable_server_pointer: true,
        request_data: None,
        autologon: true,
        enable_audio_playback: true,
        enable_audio_capture: false,
        compression_type: Some(CompressionType::Rdp61),
        pointer_software_rendering: false,
        multitransport_flags: None,
        support_dyn_vc_gfx_protocol: false,
        performance_flags: default_performance_flags(),
        desktop_scale_factor: 0,
        desktop_physical_size: None,
        desktop_orientation: None,
        device_scale_factor: None,
        monitor_extended_layout: None,
        relative_mouse_input: false,
        hardware_id: None,
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
        alternate_shell: String::new(),
        work_dir: String::new(),
        remote_application_mode: false,
        rail_support_level: RailSupportLevel::empty(),
    };

    #[cfg(windows)]
    let mut _win_clip_guard: Option<WinClipboardThreadGuard> = None;
    #[cfg(windows)]
    let mut win_clip_gate: Option<Arc<WinClipGate>> = None;
    #[cfg(target_os = "macos")]
    let mut _mac_clip_guard: Option<crate::mac_clipboard::MacClipboardGuard> = None;

    let mut connector = ClientConnector::new(config, client_addr);
    let drdynvc = DrdynvcClient::new()
        .with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new())))
        .with_dynamic_channel(ironrdp_rdpsnd::client::RdpsndDvc::new(Box::new(
            MutableRdpsndBackend::new(audio_enabled.clone()),
        )));
    connector = connector.with_static_channel(drdynvc);

    // Optional local folder redirection (RDPDR)
    let mut rdpdr_attached = false;
    if let Some(raw_folder) = params.shared_folder.as_deref() {
        if let Some(folder) = normalize_share_folder(raw_folder) {
            match build_rdpdr_channel(&folder) {
                Ok(rdpdr) => {
                    info!(session_id = %sid, folder = %folder.display(), "Attaching RDPDR shared folder");
                    connector.attach_static_channel(rdpdr);
                    rdpdr_attached = true;
                }
                Err(e) => {
                    warn!(session_id = %sid, folder = %folder.display(), %e, "Skipping RDPDR shared folder");
                }
            }
        } else if !raw_folder.trim().is_empty() {
            warn!(session_id = %sid, raw_folder, "Shared folder path invalid or missing; skipping RDPDR");
        }
    }

    // Audio playback static channel (RDPSND) ? always attached with live mute flag so users
    // can toggle sound ON/OFF mid-session without reconnecting (also satisfies [MS-RDPEFS] Appendix A).
    connector.attach_static_channel(ironrdp_rdpsnd::client::Rdpsnd::new(Box::new(
        MutableRdpsndBackend::new(audio_enabled),
    )));

    if params.clipboard_enabled {
        let proxy = SessionClipboardProxy {
            tx: input_tx.clone(),
        };
        #[cfg(windows)]
        {
            let gate = Arc::new(WinClipGate::new(session_gen, active_clip_gen.clone()));
            match spawn_win_clipboard(proxy, gate.clone()) {
                Ok((backend, guard)) => {
                    connector.attach_static_channel(ironrdp_cliprdr::Cliprdr::new(backend));
                    _win_clip_guard = Some(guard);
                    win_clip_gate = Some(gate);
                }
                Err(e) => {
                    warn!(session_id = %sid, %e, "WinClipboard init failed, falling back to stub");
                    let stub = ironrdp_cliprdr_native::StubClipboard::new();
                    let backend = stub.backend_factory().build_cliprdr_backend();
                    connector.attach_static_channel(ironrdp_cliprdr::Cliprdr::new(backend));
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            let (backend, guard) = crate::mac_clipboard::spawn_mac_clipboard(proxy);
            connector.attach_static_channel(ironrdp_cliprdr::Cliprdr::new(backend));
            _mac_clip_guard = Some(guard);
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = proxy;
            let stub = ironrdp_cliprdr_native::StubClipboard::new();
            let backend = stub.backend_factory().build_cliprdr_backend();
            connector.attach_static_channel(ironrdp_cliprdr::Cliprdr::new(backend));
        }
    }

    let mut framed = TokioFramed::new(tcp_stream);
    let should_upgrade = connect_begin(&mut framed, &mut connector)
        .await
        .context("RDP 交握失敗")?;

    let initial_stream = framed.into_inner_no_leftover();
    let (tls_stream, server_public_key) =
        upgrade_rdp_tls(initial_stream, &host, params.accept_self_signed_tls).await?;

    let upgraded = mark_as_upgraded(should_upgrade, &mut connector);
    let mut upgraded_framed = TokioFramed::new(tls_stream);
    let mut network_client = ReqwestNetworkClient::new();

    let connection_outcome = connect_finalize(
        upgraded,
        connector,
        &mut upgraded_framed,
        &mut network_client,
        ironrdp_connector::ServerName::new(&host),
        server_public_key,
        None,
    )
    .await
    .map_err(|e| {
        let raw = format!("{e:?} | {e}");
        let lower = raw.to_lowercase();
        if lower.contains("unexpected eof")
            || lower.contains("early eof")
            || lower.contains("connection reset")
            || lower.contains("broken pipe")
            || lower.contains("credssp")
            || lower.contains("logon")
            || lower.contains("credential")
            || lower.contains("access")
            || lower.contains("ntlm")
            || lower.contains("0xc000006d")
            || lower.contains("0xc000006a")
            || lower.contains("0x8009030c")
            || lower.contains("sspi")
        {
            anyhow!("[AUTH_FAILED] {raw}")
        } else {
            anyhow!("{raw}")
        }
    })?;

    let connection_result = match connection_outcome {
        ConnectionOutcome::Connected(res) => res,
        ConnectionOutcome::Redirected(redir) => {
            return Err(anyhow!("伺服器要求導向至 {:?}", redir.target_net_address));
        }
    };

    let mut fb_w = connection_result.desktop_size.width;
    let mut fb_h = connection_result.desktop_size.height;
    let mut refresh_rect_support = connection_result.refresh_rect_support;
    let mut suppress_output_support = connection_result.suppress_output_support;
    let mut input_flags = connection_result.input_flags;
    let activation_factory = connection_result.activation_factory;
    info!(
        session_id = %sid,
        host = %host,
        port = params.port,
        fb_w,
        fb_h,
        rtt_ms,
        compression = ?connection_result.compression_type,
        refresh_rect_support,
        suppress_output_support,
        input_flags = ?input_flags,
        fastpath_input = supports_fastpath_input(input_flags),
        lossless_quality = params.lossless_quality,
        enable_server_pointer = connection_result.enable_server_pointer,
        "RDP session active"
    );

    hub.send_status(&SessionStatusPayload {
        session_id: &sid,
        state: "connected",
        code: None,
        message: "已連線",
        width: fb_w,
        height: fb_h,
        rtt_ms: Some(rtt_ms),
    });

    {
        let chans: Vec<String> = connection_result
            .static_channels
            .iter_by_key()
            .map(|(_, svc)| format!("{:?}", svc.channel_name()))
            .collect();
        let ids: Vec<u16> = connection_result.static_channels.channel_ids().collect();
        info!(session_id = %sid, channels = ?chans, channel_ids = ?ids, "Static channels joined");
    }

    let mut image = DecodedImage::new(PixelFormat::RgbA32, fb_w, fb_h);
    let mut active_stage = ActiveStageBuilder {
        static_channels: connection_result.static_channels,
        user_channel_id: connection_result.user_channel_id,
        io_channel_id: connection_result.io_channel_id,
        message_channel_id: connection_result.message_channel_id,
        share_id: connection_result.share_id,
        compression_type: connection_result.compression_type,
        enable_server_pointer: connection_result.enable_server_pointer,
        pointer_software_rendering: connection_result.pointer_software_rendering,
    }
    .build();

    let (mut reader, mut writer) = split_tokio_framed(upgraded_framed);

    // Sync local CapsLock / NumLock / ScrollLock state immediately after session activation
    // (mstsc parity), guarded by server Fast-Path input support per MS-RDPBCGR 2.2.8.1.2.
    #[cfg(windows)]
    if supports_fastpath_input(input_flags) {
        let (scroll, num, caps) = crate::keyboard_hook::read_local_lock_states();
        let sync_ev = [FastPathInputEvent::SyncEvent(build_sync_flags(scroll, num, caps))];
        if let Ok(outputs) = active_stage.process_fastpath_input(&mut image, &sync_ev) {
            for out in outputs {
                if let ActiveStageOutput::ResponseFrame(frame) = out {
                    if !frame.is_empty() {
                        let _ = writer.write_all(&frame).await;
                    }
                }
            }
        }
    }

    #[cfg(windows)]
    let mut clip_init_stall: u8 = 0;
    #[cfg(windows)]
    let mut clip_init_fallback_sent = false;
    let mut pdu_log_budget: u32 = 20;
    let session_started = Instant::now();
    let mut stat_x224: u64 = 0;
    let mut stat_fastpath: u64 = 0;
    let mut stat_bytes: u64 = 0;
    let mut stat_budget: u32 = 14;
    let mut stat_tick = tokio::time::interval(Duration::from_secs(5));
    stat_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // A server that never paints anything in RemoteFX mode gets one automatic retry in
    // classic lossless mode (see `connect`), so a black screen heals itself.
    let mut no_graphics_deadline = tokio::time::Instant::now() + Duration::from_secs(NO_GRAPHICS_WAIT_SECS);
    let mut clip_timeout_tick = tokio::time::interval(Duration::from_secs(2));
    clip_timeout_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut rdpdr_poll_tick = tokio::time::interval(Duration::from_millis(80));
    rdpdr_poll_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Latest resize the UI asked for that the server has not received yet (see `SessionInput::Resize`).
    let mut pending_resize: Option<(u32, u32)> = None;
    let mut pending_resize_attempts: u32 = 0;
    let mut resize_retry_tick = tokio::time::interval(Duration::from_millis(400));
    resize_retry_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Watchdog for Windows Server hosts that hold back the initial (or post-DeactivateAll)
    // desktop paint until the client nudges with SuppressOutput toggle / RefreshRectangle.
    let mut graphics_updates_since_activate: u64 = 0;
    let mut redraw_stage: u8 = 0;
    let mut redraw_deadline = tokio::time::Instant::now() + Duration::from_millis(500);

    // Adaptive dirty-rect coalescing:
    // - Small interactive updates (typing, Bopomofo candidate bar, cursor hover) flush within ~4ms.
    // - Large drag bursts coalesce up to 12ms (8ms in RemoteFX) so horizontal strips merge cleanly.
    let mut dirty_batch = DirtyBatch::default();
    let mut dirty_since: Option<Instant> = None;
    let mut last_kbd_input_at: Option<Instant> = None;
    let max_coalesce = Duration::from_millis(if params.lossless_quality { 12 } else { 8 });
    let mut frame_flush_tick = tokio::time::interval(Duration::from_millis(4));
    frame_flush_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    'outer: loop {
        tokio::select! {
            biased;

            // Highest priority: local keyboard & mouse input must never starve behind incoming bitmaps.
            maybe_input = input_rx.recv() => {
                let Some(first_input) = maybe_input else { break 'outer; };
                let mut batch_inputs = Vec::with_capacity(8);
                batch_inputs.push(first_input);
                while batch_inputs.len() < 64 {
                    match input_rx.try_recv() {
                        Ok(next) => batch_inputs.push(next),
                        Err(_) => break,
                    }
                }

                let mut pending_fp: Vec<FastPathInputEvent> = Vec::new();
                for input in batch_inputs {
                    match input {
                        SessionInput::FastPath(events) => {
                            if events.iter().any(|ev| matches!(
                                ev,
                                FastPathInputEvent::KeyboardEvent(..)
                                    | FastPathInputEvent::UnicodeKeyboardEvent(..)
                            )) {
                                last_kbd_input_at = Some(Instant::now());
                            }
                            pending_fp.extend(events);
                        }
                        other => {
                            if !pending_fp.is_empty() {
                                let outputs = active_stage.process_fastpath_input(&mut image, &pending_fp)?;
                                pending_fp.clear();
                                for out in outputs {
                                    if let ActiveStageOutput::ResponseFrame(frame) = out {
                                        if !frame.is_empty() {
                                            writer.write_all(&frame).await?;
                                        }
                                    }
                                }
                            }
                            match other {
                                SessionInput::Disconnect => {
                                    if let Ok(frames) = active_stage.graceful_shutdown() {
                                        for out in frames {
                                            if let ActiveStageOutput::ResponseFrame(frame) = out {
                                                if !frame.is_empty() {
                                                    let _ = writer.write_all(&frame).await;
                                                }
                                            }
                                        }
                                    }
                                    break 'outer;
                                }
                                SessionInput::Clipboard(cb_msg) => {
                                    if let Some(cliprdr) = active_stage.get_svc_processor_mut::<ironrdp_cliprdr::CliprdrClient>() {
                                        let kind = match &cb_msg {
                                            ClipboardMessage::SendInitiateCopy(f) if f.is_empty() => "SendInitiateCopy(empty)",
                                            ClipboardMessage::SendInitiateCopy(_) => "SendInitiateCopy",
                                            ClipboardMessage::SendInitiateFileCopy(_) => "SendInitiateFileCopy",
                                            ClipboardMessage::SendFormatData(_) => "SendFormatData",
                                            ClipboardMessage::SendInitiatePaste(_) => "SendInitiatePaste",
                                            ClipboardMessage::SendFileContentsRequest(_) => "SendFileContentsRequest",
                                            ClipboardMessage::SendFileContentsResponse(_) => "SendFileContentsResponse",
                                            ClipboardMessage::Error(_) => "Error",
                                        };
                                        if !matches!(kind, "SendFileContentsRequest" | "SendFileContentsResponse" | "SendFormatData") {
                                            info!(session_id = %sid, kind, "CLIPRDR local message");
                                        }
                                        let svc_msgs = match cb_msg {
                                            ClipboardMessage::SendInitiateCopy(formats) => cliprdr.initiate_copy(&formats),
                                            ClipboardMessage::SendInitiateFileCopy(files) => cliprdr.initiate_file_copy(files),
                                            ClipboardMessage::SendFormatData(resp) => cliprdr.submit_format_data(resp),
                                            ClipboardMessage::SendInitiatePaste(format) => cliprdr.initiate_paste(format),
                                            ClipboardMessage::SendFileContentsRequest(req) => cliprdr.request_file_contents(req),
                                            ClipboardMessage::SendFileContentsResponse(resp) => cliprdr.submit_file_contents(resp),
                                            ClipboardMessage::Error(e) => {
                                                warn!(session_id = %sid, %e, "Clipboard backend error");
                                                continue;
                                            }
                                        };
                                        match svc_msgs {
                                            Ok(msgs) => {
                                                match active_stage.process_svc_processor_messages(msgs) {
                                                    Ok(frame) => {
                                                        if !frame.is_empty() {
                                                            if let Err(e) = writer.write_all(&frame).await {
                                                                warn!(session_id = %sid, kind, %e, "Failed writing CLIPRDR frame");
                                                            }
                                                        }
                                                    }
                                                    Err(e) => {
                                                        warn!(session_id = %sid, kind, %e, "Failed encoding CLIPRDR SVC messages");
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                warn!(session_id = %sid, kind, %e, "CLIPRDR rejected outgoing clipboard message");
                                            }
                                        }
                                    }
                                }
                                SessionInput::Resize { width, height } => {
                                    let w = u32::from(width.clamp(640, 4096) & !1);
                                    let h = u32::from(height.clamp(480, 2160) & !1);
                                    // The display-control channel opens a moment after the session
                                    // becomes active. A request made before that used to be dropped
                                    // silently; keep it and retry until the server has it.
                                    pending_resize = Some((w, h));
                                    pending_resize_attempts = 0;
                                    match active_stage.encode_resize(w, h, Some(100), None) {
                                        Some(Ok(frame)) => {
                                            if !frame.is_empty() {
                                                writer.write_all(&frame).await.context("寫入 RDP 資料失敗")?;
                                            }
                                            info!(session_id = %sid, w, h, "Resize request sent");
                                            pending_resize = None;
                                        }
                                        Some(Err(e)) => {
                                            warn!(session_id = %sid, w, h, %e, "Resize request could not be encoded; will retry");
                                        }
                                        None => {
                                            info!(session_id = %sid, w, h, "Display control channel not ready; resize queued");
                                        }
                                    }
                                }
                                SessionInput::FastPath(_) => unreachable!(),
                            }
                        }
                    }
                }
                if !pending_fp.is_empty() {
                    let outputs = active_stage.process_fastpath_input(&mut image, &pending_fp)?;
                    for out in outputs {
                        if let ActiveStageOutput::ResponseFrame(frame) = out {
                            if !frame.is_empty() {
                                writer.write_all(&frame).await?;
                            }
                        }
                    }
                }
            }

            frame_res = reader.read_pdu() => {
                let (action, payload) = match frame_res {
                    Ok(v) => v,
                    Err(e) => {
                        debug!(session_id = %sid, %e, "RDP read stream ended");
                        return Err(anyhow!("連線已中斷：{e}"));
                    }
                };

                let outputs = active_stage
                    .process(&mut image, action, &payload, None)
                    .context("RDP 資料處理失敗")?;
                stat_bytes += payload.len() as u64;
                if matches!(action, ironrdp_pdu::Action::FastPath) {
                    stat_fastpath += 1;
                } else {
                    stat_x224 += 1;
                }
                if pdu_log_budget > 0 {
                    pdu_log_budget -= 1;
                    info!(
                        session_id = %sid,
                        action = ?action,
                        payload_len = payload.len(),
                        outputs = outputs.len(),
                        "PDU received"
                    );
                }

                // Keep Windows clipboard listener gate synchronized with CLIPRDR state machine so
                // WM_CLIPBOARDUPDATE is only forwarded once ServerCapabilities + MonitorReady +
                // FormatListResponse::Ok have completed.
                #[cfg(windows)]
                if let Some(ref gate) = win_clip_gate {
                    if let Some(cliprdr) = active_stage.get_svc_processor_mut::<ironrdp_cliprdr::CliprdrClient>() {
                        let is_ready = cliprdr.is_ready();
                        let prev = gate.ready.swap(is_ready, Ordering::Release);
                        if is_ready && !prev {
                            info!(session_id = %sid, "CLIPRDR channel is ready");
                        } else if !is_ready && prev {
                            info!(session_id = %sid, "CLIPRDR channel left ready state");
                        }
                    }
                }

                // If IronRDP discarded a malformed/truncated Fast-Path bitmap, immediately request
                // a full visual recovery redraw from the server.
                if active_stage.take_bitmap_recovery_request() {
                    warn!(session_id = %sid, "Bitmap recovery requested by IronRDP; sending full redraw");
                    if let Ok(frames) = active_stage.request_full_redraw(
                        fb_w,
                        fb_h,
                        refresh_rect_support,
                        suppress_output_support,
                    ) {
                        for frame in frames {
                            if !frame.is_empty() {
                                let _ = writer.write_all(&frame).await;
                            }
                        }
                    }
                }

                for out in outputs {
                    match out {
                        ActiveStageOutput::ResponseFrame(frame) => {
                            if !frame.is_empty() {
                                writer.write_all(&frame).await.context("寫入 RDP 資料失敗")?;
                                // A non-empty response frame right after graphics updates (e.g. FrameMarker::End -> FrameAck)
                                // signals a complete remote frame boundary; flush immediately for lowest latency!
                                if !dirty_batch.is_empty() {
                                    dirty_batch.flush(&hub, &sid, &image);
                                    dirty_since = None;
                                }
                            }
                        }
                        ActiveStageOutput::GraphicsUpdate(region) => {
                            graphics_updates_since_activate = graphics_updates_since_activate.saturating_add(1);
                            if graphics_updates_since_activate == 1 {
                                info!(
                                    session_id = %sid,
                                    left = region.left,
                                    top = region.top,
                                    right = region.right,
                                    bottom = region.bottom,
                                    "First graphics update received"
                                );
                            }
                            if dirty_batch.is_empty() {
                                dirty_since = Some(Instant::now());
                            }
                            dirty_batch.push(region.left, region.top, region.right, region.bottom);
                        }
                        ActiveStageOutput::PointerDefault => {
                            hub.send_pointer_default(&sid);
                        }
                        ActiveStageOutput::PointerHidden => {
                            hub.send_pointer_hidden(&sid);
                        }
                        ActiveStageOutput::PointerBitmap(pointer) => {
                            hub.send_pointer_bitmap(
                                &sid,
                                pointer.width,
                                pointer.height,
                                pointer.hotspot_x,
                                pointer.hotspot_y,
                                &pointer.bitmap_data,
                            );
                        }
                        ActiveStageOutput::SaveSessionInfo { logon_complete } => {
                            info!(
                                session_id = %sid,
                                logon_complete,
                                graphics_updates = graphics_updates_since_activate,
                                "Server SaveSessionInfo notification"
                            );
                            if logon_complete && graphics_updates_since_activate == 0 {
                                if let Ok(frames) = active_stage.request_full_redraw(
                                    fb_w,
                                    fb_h,
                                    refresh_rect_support,
                                    suppress_output_support,
                                ) {
                                    for frame in frames {
                                        if !frame.is_empty() {
                                            let _ = writer.write_all(&frame).await;
                                        }
                                    }
                                }
                            }
                        }
                        ActiveStageOutput::LogonErrors(err_info) => {
                            warn!(session_id = %sid, ?err_info, "Server sent LogonErrors notification");
                        }
                        ActiveStageOutput::AutoReconnectFailed => {
                            warn!(session_id = %sid, "Server reported AutoReconnectFailed");
                        }
                        ActiveStageOutput::ServerRedirection(redir) => {
                            let target = redir
                                .target_net_address
                                .or(redir.target_fqdn)
                                .unwrap_or_else(|| format!("session {}", redir.session_id));
                            warn!(session_id = %sid, %target, "Server sent post-activation ServerRedirection");
                            return Err(anyhow!("伺服器要求導向至 {target}"));
                        }
                        ActiveStageOutput::WindowingOrders(orders) => {
                            debug!(session_id = %sid, count = orders.len(), "Received WindowingOrders (unused in desktop mode)");
                        }
                        ActiveStageOutput::WindowSurfaceUpdates(updates) => {
                            debug!(session_id = %sid, count = updates.len(), "Received WindowSurfaceUpdates (unused in desktop mode)");
                        }
                        ActiveStageOutput::DeactivateAll => {
                            info!(session_id = %sid, "Received Server DeactivateAll; starting reactivation");
                            dirty_batch.clear();
                            dirty_since = None;
                            hub.send_pointer_default(&sid);
                            let mut connection_activation = activation_factory.create();
                            let mut buf = ironrdp_core::WriteBuf::new();
                            loop {
                                buf.clear();
                                let written = single_sequence_step_read(
                                    &mut reader,
                                    &mut connection_activation,
                                    &mut buf,
                                )
                                .await
                                .context("重新啟用連線失敗")?;

                                if written.size().is_some() {
                                    writer.write_all(buf.filled()).await?;
                                }
                                match connection_activation.connection_activation_state() {
                                    ironrdp_connector::connection_activation::ConnectionActivationState::Finalized {
                                        desktop_size,
                                        share_id,
                                        input_flags: new_input_flags,
                                        static_channel_chunk_size,
                                        enable_server_pointer,
                                        pointer_software_rendering,
                                        refresh_rect_support: new_refresh_rect,
                                        suppress_output_support: new_suppress_output,
                                        ..
                                    } => {
                                        fb_w = desktop_size.width;
                                        fb_h = desktop_size.height;
                                        input_flags = new_input_flags;
                                        refresh_rect_support = new_refresh_rect;
                                        suppress_output_support = new_suppress_output;
                                        graphics_updates_since_activate = 0;
                                        redraw_stage = 0;
                                        redraw_deadline = tokio::time::Instant::now() + Duration::from_millis(400);
                                        no_graphics_deadline = tokio::time::Instant::now() + Duration::from_secs(NO_GRAPHICS_WAIT_SECS);

                                        image = DecodedImage::new(PixelFormat::RgbA32, fb_w, fb_h);
                                        active_stage.reactivate(
                                            connection_activation.io_channel_id(),
                                            connection_activation.user_channel_id(),
                                            share_id,
                                            enable_server_pointer,
                                            pointer_software_rendering,
                                            static_channel_chunk_size,
                                        );
                                        info!(
                                            session_id = %sid,
                                            fb_w,
                                            fb_h,
                                            refresh_rect_support,
                                            suppress_output_support,
                                            input_flags = ?input_flags,
                                            "RDP session reactivated"
                                        );
                                        hub.send_status(&SessionStatusPayload {
                                            session_id: &sid,
                                            state: "connected",
                                            code: None,
                                            message: "已連線",
                                            width: fb_w,
                                            height: fb_h,
                                            rtt_ms: Some(rtt_ms),
                                        });
                                        break;
                                    }
                                    ironrdp_connector::connection_activation::ConnectionActivationState::Redirected {
                                        redirection,
                                    } => {
                                        let target = redirection
                                            .target_net_address
                                            .or(redirection.target_fqdn)
                                            .unwrap_or_else(|| format!("session {}", redirection.session_id));
                                        warn!(session_id = %sid, %target, "Server redirected during reactivation");
                                        return Err(anyhow!("伺服器要求導向至 {target}"));
                                    }
                                    _ => {}
                                }
                            }
                        }
                        ActiveStageOutput::Terminate(reason) => {
                            let r_str = format!("{reason:?}");
                            info!(session_id = %sid, reason = %r_str, "Server terminated RDP session");
                            let lower = r_str.to_lowercase();
                            if lower.contains("logon") || lower.contains("credential") || lower.contains("password") {
                                return Err(anyhow!("[AUTH_FAILED] {r_str}"));
                            }
                            break 'outer;
                        }
                        _ => {}
                    }
                }
            }

            _ = tokio::time::sleep_until(redraw_deadline), if graphics_updates_since_activate == 0 && redraw_stage < 2 => {
                let prefer_suppress = redraw_stage == 0;
                redraw_stage += 1;
                if redraw_stage == 1 {
                    redraw_deadline = tokio::time::Instant::now() + Duration::from_millis(1300);
                }
                let frames = if prefer_suppress {
                    active_stage.request_full_redraw(
                        fb_w,
                        fb_h,
                        refresh_rect_support,
                        suppress_output_support,
                    )
                } else if refresh_rect_support {
                    active_stage.request_full_redraw(fb_w, fb_h, true, false)
                } else {
                    active_stage.request_full_redraw(
                        fb_w,
                        fb_h,
                        refresh_rect_support,
                        suppress_output_support,
                    )
                };
                match frames {
                    Ok(frames) => {
                        let mut sent_bytes = 0usize;
                        for frame in frames {
                            if !frame.is_empty() {
                                sent_bytes += frame.len();
                                if let Err(e) = writer.write_all(&frame).await {
                                    warn!(session_id = %sid, %e, "Failed writing full-redraw request");
                                    break;
                                }
                            }
                        }
                        info!(
                            session_id = %sid,
                            stage = redraw_stage,
                            sent_bytes,
                            refresh_rect_support,
                            suppress_output_support,
                            "No graphics updates yet; requested full redraw from server"
                        );
                    }
                    Err(e) => {
                        warn!(session_id = %sid, %e, "Failed encoding full-redraw request");
                    }
                }
            }
            _ = tokio::time::sleep_until(no_graphics_deadline), if graphics_updates_since_activate == 0 && !params.lossless_quality => {
                warn!(session_id = %sid, "No graphics from server in RemoteFX mode; asking caller to retry in classic lossless mode");
                return Err(anyhow!("{NO_GRAPHICS_MARK} server sent no graphics in RemoteFX mode"));
            }
            _ = frame_flush_tick.tick(), if !dirty_batch.is_empty() => {
                let area = dirty_batch.total_area();
                let recent_kbd = last_kbd_input_at
                    .is_some_and(|t| t.elapsed() < Duration::from_millis(250));
                let elapsed = dirty_since.map_or(max_coalesce, |t| t.elapsed());
                if should_flush_dirty_batch(area, recent_kbd, elapsed, max_coalesce) {
                    dirty_batch.flush(&hub, &sid, &image);
                    dirty_since = None;
                }
            }

            _ = stat_tick.tick(), if stat_budget > 0 => {
                stat_budget -= 1;
                #[cfg(windows)]
                let clip_state = win_clip_gate
                    .as_ref()
                    .map(|g| format!(
                        "monitor_ready={} ready={}",
                        g.monitor_ready.load(Ordering::Acquire),
                        g.ready.load(Ordering::Acquire)
                    ))
                    .unwrap_or_else(|| "off".to_owned());
                #[cfg(windows)]
                info!(session_id = %sid, clipboard = %clip_state, "Clipboard state");
                info!(
                    session_id = %sid,
                    secs = session_started.elapsed().as_secs(),
                    x224_pdus = stat_x224,
                    fastpath_pdus = stat_fastpath,
                    bytes = stat_bytes,
                    graphics_updates = graphics_updates_since_activate,
                    "Traffic summary"
                );
            }

            _ = clip_timeout_tick.tick() => {
                // The channel only becomes Ready after the server answered our initial format
                // list. If MonitorReady arrived but that list never went out (host clipboard busy
                // or the local backend gave up), send an empty one ourselves so both directions
                // unlock instead of staying dead for the whole session.
                #[cfg(windows)]
                if let Some(ref gate) = win_clip_gate {
                    if gate.monitor_ready.load(Ordering::Acquire)
                        && !gate.ready.load(Ordering::Acquire)
                        && !clip_init_fallback_sent
                    {
                        clip_init_stall = clip_init_stall.saturating_add(1);
                        if clip_init_stall >= 2 {
                            clip_init_fallback_sent = true;
                            if let Some(cliprdr) = active_stage.get_svc_processor_mut::<ironrdp_cliprdr::CliprdrClient>() {
                                if !cliprdr.is_ready() {
                                    match cliprdr.initiate_copy(&[]) {
                                        Ok(msgs) => match active_stage.process_svc_processor_messages(msgs) {
                                            Ok(frame) => {
                                                if !frame.is_empty() {
                                                    let _ = writer.write_all(&frame).await;
                                                }
                                                info!(session_id = %sid, "CLIPRDR initial format list was stalled; sent an empty one to finish initialization");
                                            }
                                            Err(e) => warn!(session_id = %sid, %e, "CLIPRDR fallback encode failed"),
                                        },
                                        Err(e) => warn!(session_id = %sid, %e, "CLIPRDR fallback initiate_copy rejected"),
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(cliprdr) = active_stage.get_svc_processor_mut::<ironrdp_cliprdr::CliprdrClient>() {
                    if let Ok(msgs) = cliprdr.drive_timeouts() {
                        if let Ok(frame) = active_stage.process_svc_processor_messages(msgs) {
                            if !frame.is_empty() {
                                let _ = writer.write_all(&frame).await;
                            }
                        }
                    }
                }
            }

            _ = resize_retry_tick.tick(), if pending_resize.is_some() => {
                if let Some((w, h)) = pending_resize {
                    pending_resize_attempts += 1;
                    match active_stage.encode_resize(w, h, Some(100), None) {
                        Some(Ok(frame)) => {
                            if !frame.is_empty() {
                                writer.write_all(&frame).await.context("寫入 RDP 資料失敗")?;
                            }
                            info!(session_id = %sid, w, h, attempts = pending_resize_attempts, "Queued resize request sent");
                            pending_resize = None;
                        }
                        // ~30 s of retries: a server without display control never resizes.
                        _ if pending_resize_attempts >= 75 => {
                            warn!(session_id = %sid, w, h, "Server never opened the display control channel; giving up on resize");
                            pending_resize = None;
                        }
                        _ => {}
                    }
                }
            }

            _ = rdpdr_poll_tick.tick(), if rdpdr_attached => {
                if let Some(rdpdr) = active_stage.get_svc_processor_mut::<ironrdp_rdpdr::Rdpdr>() {
                    if let Ok(msgs) = rdpdr.poll_deferred_messages() {
                        if !msgs.is_empty() {
                            let wrapped = ironrdp_svc::SvcProcessorMessages::<ironrdp_rdpdr::Rdpdr>::new(msgs);
                            if let Ok(frame) = active_stage.process_svc_processor_messages(wrapped) {
                                if !frame.is_empty() {
                                    let _ = writer.write_all(&frame).await;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Coalesces overlapping or adjacent RDP dirty rectangles into a compact batch so
/// window drags and full-screen switches are pushed as one atomic frame instead of
/// dozens of fragmented horizontal strips.
#[derive(Default)]
struct DirtyBatch {
    rects: Vec<(u16, u16, u16, u16)>, // inclusive (left, top, right, bottom)
}

impl DirtyBatch {
    const MAX_RECTS: usize = 6;

    fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    fn clear(&mut self) {
        self.rects.clear();
    }

    fn total_area(&self) -> u64 {
        self.rects.iter().map(|&r| Self::rect_area(r)).sum()
    }

    fn rect_area(r: (u16, u16, u16, u16)) -> u64 {
        u64::from(r.2 - r.0 + 1) * u64::from(r.3 - r.1 + 1)
    }

    fn rect_union(a: (u16, u16, u16, u16), b: (u16, u16, u16, u16)) -> (u16, u16, u16, u16) {
        (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
    }

    fn should_merge(a: (u16, u16, u16, u16), b: (u16, u16, u16, u16)) -> bool {
        let u = Self::rect_union(a, b);
        let ua = Self::rect_area(u);
        let sum = Self::rect_area(a) + Self::rect_area(b);
        // Always merge touching/overlapping or tightly adjacent strips with <= 35% overhead
        let expanded_a = (
            a.0.saturating_sub(2),
            a.1.saturating_sub(2),
            a.2.saturating_add(2),
            a.3.saturating_add(2),
        );
        let touches = expanded_a.0 <= b.2
            && expanded_a.2 >= b.0
            && expanded_a.1 <= b.3
            && expanded_a.3 >= b.1;
        touches && ua <= (sum * 135 / 100) + 4096
    }

    fn push(&mut self, left: u16, top: u16, right: u16, bottom: u16) {
        if right < left || bottom < top {
            return;
        }
        let mut cur = (left, top, right, bottom);
        loop {
            let mut merged = false;
            for i in 0..self.rects.len() {
                if Self::should_merge(self.rects[i], cur) {
                    cur = Self::rect_union(self.rects[i], cur);
                    self.rects.swap_remove(i);
                    merged = true;
                    break;
                }
            }
            if !merged {
                break;
            }
        }
        self.rects.push(cur);

        // Bound maximum disjoint rects by merging the pair with minimum wasted area
        while self.rects.len() > Self::MAX_RECTS {
            let mut best = (0usize, 1usize, u64::MAX);
            for i in 0..self.rects.len() {
                for j in (i + 1)..self.rects.len() {
                    let u = Self::rect_union(self.rects[i], self.rects[j]);
                    let waste = Self::rect_area(u)
                        .saturating_sub(Self::rect_area(self.rects[i]) + Self::rect_area(self.rects[j]));
                    if waste < best.2 {
                        best = (i, j, waste);
                    }
                }
            }
            let merged = Self::rect_union(self.rects[best.0], self.rects[best.1]);
            self.rects.swap_remove(best.1);
            self.rects[best.0] = merged;
        }
    }

    fn flush(&mut self, hub: &StreamHub, sid: &str, image: &DecodedImage) {
        if self.rects.is_empty() {
            return;
        }
        let fb_w = image.width();
        let fb_h = image.height();
        let stride = usize::from(fb_w) * 4;
        let src = image.data();

        let mut packed_rects: Vec<(u16, u16, u16, u16, Vec<u8>)> = Vec::with_capacity(self.rects.len());
        for &(left, top, right, bottom) in &self.rects {
            if left >= fb_w || top >= fb_h || right < left || bottom < top {
                continue;
            }
            let clamped_r = right.min(fb_w - 1);
            let clamped_b = bottom.min(fb_h - 1);
            let rw = clamped_r - left + 1;
            let rh = clamped_b - top + 1;
            let row_bytes = usize::from(rw) * 4;
            let mut packed = Vec::with_capacity(row_bytes * usize::from(rh));
            for y in top..=clamped_b {
                let start = usize::from(y) * stride + usize::from(left) * 4;
                let end = start + row_bytes;
                if end <= src.len() {
                    packed.extend_from_slice(&src[start..end]);
                }
            }
            packed_rects.push((left, top, rw, rh, packed));
        }
        self.rects.clear();

        if !packed_rects.is_empty() {
            hub.send_rgba_batch(sid, fb_w, fb_h, &packed_rects);
        }
    }
}

/// Decide whether to flush accumulated dirty rectangles on the 4ms tick:
/// flush immediately for small/interactive updates (typing, IME popup, hover),
/// while coalescing large drag bursts up to `max_coalesce` to keep frames tear-free.
fn should_flush_dirty_batch(
    area: u64,
    recent_kbd: bool,
    elapsed: Duration,
    max_coalesce: Duration,
) -> bool {
    area <= 180_000 || (recent_kbd && area <= 350_000) || elapsed >= max_coalesce
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn performance_flags_allow_full_window_drag() {
        let flags = default_performance_flags();
        assert!(!flags.contains(PerformanceFlags::DISABLE_FULLWINDOWDRAG));
        assert!(flags.contains(PerformanceFlags::ENABLE_FONT_SMOOTHING));
        assert!(flags.contains(PerformanceFlags::ENABLE_DESKTOP_COMPOSITION));
    }

    #[test]
    fn bitmap_config_switches_between_lossless_and_remotefx() {
        assert!(bitmap_config_for_quality(true).is_none());
        let cfg = bitmap_config_for_quality(false).expect("RemoteFX bitmap config");
        assert_eq!(cfg.color_depth, 32);
        assert!(cfg.lossy_compression);
        assert!(!cfg.codecs.0.is_empty(), "RemoteFX codec should be advertised");
    }

    #[test]
    fn adaptive_dirty_flush_prioritizes_typing_and_coalesces_large_drags() {
        let max_w = Duration::from_millis(12);
        // Small Bopomofo / caret redraw flushes right away at 4ms
        assert!(should_flush_dirty_batch(16_000, false, Duration::from_millis(4), max_w));
        // Medium IME candidate window during typing flushes right away at 4ms
        assert!(should_flush_dirty_batch(260_000, true, Duration::from_millis(4), max_w));
        // Large window drag waits to coalesce until 12ms window elapses
        assert!(!should_flush_dirty_batch(900_000, false, Duration::from_millis(4), max_w));
        assert!(should_flush_dirty_batch(900_000, false, Duration::from_millis(12), max_w));
    }

    #[test]
    fn dirty_batch_merges_adjacent_strips_and_keeps_distant_updates_separate() {
        let mut batch = DirtyBatch::default();
        // Simulate 4 horizontal 64px scanline strips from a dragged window
        batch.push(100, 0, 899, 63);
        batch.push(100, 64, 899, 127);
        batch.push(100, 128, 899, 191);
        batch.push(100, 192, 899, 255);
        assert_eq!(batch.rects.len(), 1);
        assert_eq!(batch.rects[0], (100, 0, 899, 255));

        // Distant tiny taskbar clock tick should stay separate instead of full-screen blowup
        batch.push(1850, 1040, 1919, 1079);
        assert_eq!(batch.rects.len(), 2);
    }

    #[test]
    fn fastpath_input_flag_check_detects_supported_modes() {
        assert!(!supports_fastpath_input(InputFlags::empty()));
        assert!(!supports_fastpath_input(InputFlags::SCANCODES | InputFlags::UNICODE));
        assert!(supports_fastpath_input(InputFlags::FASTPATH_INPUT));
        assert!(supports_fastpath_input(InputFlags::FASTPATH_INPUT_2));
        assert!(supports_fastpath_input(
            InputFlags::FASTPATH_INPUT | InputFlags::FASTPATH_INPUT_2
        ));
    }

    #[cfg(windows)]
    #[test]
    fn win_clip_gate_blocks_until_ready_and_blocks_self_and_inactive_tabs() {
        let active = Arc::new(AtomicU64::new(1));
        let tab1 = WinClipGate::new(1, active.clone());
        let tab2 = WinClipGate::new(2, active.clone());

        // 1) Before CLIPRDR MonitorReady + FormatListResponse::Ok, NEVER forward WM_CLIPBOARDUPDATE
        assert!(!tab1.should_forward_clipboard_update(false));
        assert!(!tab2.should_forward_clipboard_update(false));

        // 1b) MonitorReady alone (channel still initializing, e.g. the initial format list never
        //     went out) must already unlock host->remote copy; otherwise the channel deadlocks.
        tab1.monitor_ready.store(true, Ordering::Release);
        assert!(tab1.should_forward_clipboard_update(false));
        assert!(!tab2.should_forward_clipboard_update(false));
        tab1.monitor_ready.store(false, Ordering::Release);

        // 2) Mark both tabs ready
        tab1.ready.store(true, Ordering::Release);
        tab2.ready.store(true, Ordering::Release);

        // 3) External host update (screenshot / Notepad / Explorer): only active tab forwards
        assert!(tab1.should_forward_clipboard_update(false));
        assert!(!tab2.should_forward_clipboard_update(false));

        // 4) MistRDP's own clipboard write (SetClipboardData or OleSetClipboard for remote files):
        //    BOTH tabs MUST ignore it so remote->host files are never stolen or echoed back!
        assert!(!tab1.should_forward_clipboard_update(true));
        assert!(!tab2.should_forward_clipboard_update(true));

        // 5) Switching active tab to tab2 hands host->remote forwarding cleanly to tab2
        active.store(2, Ordering::Release);
        assert!(!tab1.should_forward_clipboard_update(false));
        assert!(tab2.should_forward_clipboard_update(false));
    }

    #[cfg(windows)]
    #[test]
    fn win_clipboard_subclass_blocks_premature_updates_and_cleans_up_arc() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_CLIPBOARDUPDATE};

        let (tx, mut rx) = mpsc::unbounded_channel();
        let proxy = SessionClipboardProxy { tx };
        let active = Arc::new(AtomicU64::new(42));
        let gate = Arc::new(WinClipGate::new(42, active));
        let gate_for_thread = gate.clone();

        let join = std::thread::spawn(move || {
            let com_hr = unsafe {
                windows_sys::Win32::System::Com::CoInitializeEx(
                    std::ptr::null(),
                    windows_sys::Win32::System::Com::COINIT_APARTMENTTHREADED as u32,
                )
            };
            let com_inited = com_hr >= 0;
            let tid = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
            let win_cb = ironrdp_cliprdr_native::WinClipboard::with_file_transfer(proxy, true)
                .expect("WinClipboard should initialize on STA thread");
            let (hwnd, raw_gate) = patch_win_clipboard_always_listen(tid, gate_for_thread)
                .expect("IronRDPClipboardMonitor window should be found and subclassed");

            // While gate.ready == false (Initialization phase), WM_CLIPBOARDUPDATE MUST be absorbed
            // and never emit a premature SendInitiateCopy PDU.
            unsafe {
                SendMessageW(hwnd, WM_CLIPBOARDUPDATE, 0, 0);
            }

            unpatch_win_clipboard_always_listen(hwnd, raw_gate);
            drop(win_cb);

            if com_inited {
                unsafe { windows_sys::Win32::System::Com::CoUninitialize() };
            }
        });

        join.join().expect("clipboard subclass test thread must not panic");
        assert!(
            rx.try_recv().is_err(),
            "Premature WM_CLIPBOARDUPDATE before CLIPRDR Ready must not emit ClipboardMessage"
        );
        assert_eq!(
            Arc::strong_count(&gate),
            1,
            "WinClipGate Arc reference must be reclaimed cleanly on unpatch"
        );
    }

    #[test]
    fn build_sync_flags_sets_requested_locks() {
        assert_eq!(build_sync_flags(false, false, false), SynchronizeFlags::empty());
        let f = build_sync_flags(true, true, false);
        assert!(f.contains(SynchronizeFlags::SCROLL_LOCK));
        assert!(f.contains(SynchronizeFlags::NUM_LOCK));
        assert!(!f.contains(SynchronizeFlags::CAPS_LOCK));
        let all = build_sync_flags(true, true, true);
        assert!(all.contains(SynchronizeFlags::CAPS_LOCK));
    }

    #[test]
    fn classify_auth_failure_errors() {
        let (code1, msg1) = classify_error_text("CredSSP error: STATUS_LOGON_FAILURE (0xC000006D)");
        assert_eq!(code1, "AUTH_FAILED");
        assert!(msg1.contains("帳號或密碼錯誤"));

        let (code2, msg2) = classify_error_text("NTSTATUS 0xc0000234 account_locked_out");
        assert_eq!(code2, "AUTH_FAILED");
        assert!(msg2.contains("鎖定"));

        let (code3, _) = classify_error_text("[AUTH_FAILED] unexpected eof");
        assert_eq!(code3, "AUTH_FAILED");

        let (code4, msg4) = classify_error_text("TLS 連線失敗：schannel handshake error | detail");
        assert_eq!(code4, "TLS_ERROR");
        assert!(msg4.contains("schannel handshake error"));
    }

    fn der_tlv(tag: u8, value: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        let len = value.len();
        if len < 0x80 {
            out.push(len as u8);
        } else if len <= 0xFF {
            out.push(0x81);
            out.push(len as u8);
        } else {
            out.push(0x82);
            out.push((len >> 8) as u8);
            out.push((len & 0xFF) as u8);
        }
        out.extend_from_slice(value);
        out
    }

    #[test]
    fn extract_spki_handles_windows_self_signed_v3_and_v1_certs() {
        let fake_pubkey: Vec<u8> = (0u8..140u8).collect();
        let mut bit_str = vec![0u8]; // 0 unused bits
        bit_str.extend_from_slice(&fake_pubkey);

        let alg_id = der_tlv(0x30, &[0x06, 0x03, 0x2A, 0x86, 0x48]);
        let spki = der_tlv(0x30, &[alg_id.clone(), der_tlv(0x03, &bit_str)].concat());

        // Non-canonical PrintableString containing '_' (common in Windows LAN NetBIOS CNs)
        let bad_cn = der_tlv(0x13, b"WIN_LAN_SERVER_01");
        let name = der_tlv(0x30, &bad_cn);
        let serial = der_tlv(0x02, &[0x01, 0x02, 0x03]);
        let validity = der_tlv(0x30, &[0x17, 0x02, b'0', b'0']);

        // v3 cert with explicit [0] version tag
        let version_v3 = der_tlv(0xA0, &der_tlv(0x02, &[0x02]));
        let tbs_v3 = der_tlv(
            0x30,
            &[
                version_v3,
                serial.clone(),
                alg_id.clone(),
                name.clone(),
                validity.clone(),
                name.clone(),
                spki.clone(),
            ]
            .concat(),
        );
        let cert_v3 = der_tlv(0x30, &[tbs_v3, alg_id.clone(), der_tlv(0x03, &[0x00, 0xAA])].concat());
        assert_eq!(
            extract_spki_public_key_from_cert_der(&cert_v3),
            Some(fake_pubkey.clone())
        );

        // v1 cert without optional [0] version tag
        let tbs_v1 = der_tlv(
            0x30,
            &[serial, alg_id.clone(), name.clone(), validity, name, spki].concat(),
        );
        let cert_v1 = der_tlv(0x30, &[tbs_v1, alg_id, der_tlv(0x03, &[0x00, 0xBB])].concat());
        assert_eq!(extract_spki_public_key_from_cert_der(&cert_v1), Some(fake_pubkey));

        // Reject garbage / truncated DER cleanly
        assert_eq!(extract_spki_public_key_from_cert_der(&[]), None);
        assert_eq!(extract_spki_public_key_from_cert_der(&[0x30, 0x80, 0x00]), None);
    }

    #[test]
    fn sanitize_drive_name_filters_to_safe_ascii() {
        assert_eq!(sanitize_drive_name(Path::new("/home/user/Downloads")), "Downloads");
        assert_eq!(sanitize_drive_name(Path::new("/tmp/我的資料夾")), "MistShare");
        assert_eq!(sanitize_drive_name(Path::new("/tmp/my share_01!")), "myshare_01");
    }

    #[test]
    fn normalize_share_folder_accepts_existing_temp_dir_and_rejects_missing() {
        let tmp = std::env::temp_dir();
        let norm = normalize_share_folder(&tmp.to_string_lossy()).expect("temp dir should exist");
        assert!(norm.is_dir());
        assert!(!norm.to_string_lossy().starts_with(r"\\?\"));
        assert!(normalize_share_folder("   ").is_none());
        assert!(normalize_share_folder("/path/that/does/not/exist/mistrdp_xyz").is_none());
    }

    #[test]
    fn rdp_connect_from_non_tokio_thread_does_not_panic() {
        let closed_port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let hub = rt.block_on(StreamHub::start()).unwrap();
        let mut rx = hub.subscribe_raw();
        let mgr = RdpManager::new(hub, rt.handle().clone());

        let mgr_clone = mgr.clone();
        let join = std::thread::spawn(move || {
            mgr_clone.connect(ConnectParams {
                session_id: "test-non-tokio".to_string(),
                host: "127.0.0.1".to_string(),
                port: closed_port,
                username: "tester".to_string(),
                domain: None,
                password: "pw".to_string(),
                width: 1280,
                height: 720,
                clipboard_enabled: false,
                audio_enabled: false,
                lossless_quality: true,
                shared_folder: None,
                accept_self_signed_tls: true,
            })
        });

        let connect_res = join.join().expect("connect() must never panic off-runtime");
        assert!(connect_res.is_ok());

        let saw_error = rt.block_on(async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while tokio::time::Instant::now() < deadline {
                if let Ok(Ok(frame)) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await
                {
                    if frame.first() == Some(&0x03) {
                        let txt = String::from_utf8_lossy(&frame[1..]);
                        if txt.contains("\"test-non-tokio\"") && txt.contains("\"error\"") {
                            return true;
                        }
                    }
                }
            }
            false
        });
        assert!(saw_error, "expected RDP task to report TCP connect error over StreamHub");
    }

    #[test]
    fn ws_input_frames_become_ordered_key_events() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let hub = rt.block_on(StreamHub::start()).unwrap();
        let mgr = RdpManager::new(hub, rt.handle().clone());
        let (tx, mut rx) = mpsc::unbounded_channel();
        mgr.sessions.lock().unwrap().insert(
            "s1".into(),
            SessionHandle { generation: 1, tx, audio_enabled: Arc::new(AtomicBool::new(true)) },
        );
        mgr.handle_ws_input(&[0x10, 2, 0, b's', b'1', 0x1E, 2]);
        mgr.handle_ws_input(&[0x10, 2, 0, b's', b'1', 0x1E, 0]);
        mgr.handle_ws_input(&[0x11, 2, 0, b's', b'1', 0x41, 0x00, 1]);
        mgr.handle_ws_input(&[0x10, 9, 0, b's', b'1']); // truncated: ignored
        let mut got = Vec::new();
        while let Ok(SessionInput::FastPath(ev)) = rx.try_recv() {
            got.push(ev);
        }
        assert_eq!(got.len(), 3);
        assert!(matches!(got[0][0], FastPathInputEvent::KeyboardEvent(f, 0x1E) if !f.contains(KeyboardFlags::RELEASE)));
        assert!(matches!(got[1][0], FastPathInputEvent::KeyboardEvent(f, 0x1E) if f.contains(KeyboardFlags::RELEASE)));
        assert!(matches!(got[2][0], FastPathInputEvent::UnicodeKeyboardEvent(_, 0x41)));
    }

    #[test]
    fn same_session_id_generation_prevents_stale_eviction() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let hub = rt.block_on(StreamHub::start()).unwrap();
        let mgr = RdpManager::new(hub, rt.handle().clone());

        let (tx1, _rx1) = mpsc::unbounded_channel();
        let (tx2, _rx2) = mpsc::unbounded_channel();
        let gen1 = NEXT_SESSION_GEN.fetch_add(1, Ordering::Relaxed);
        let gen2 = NEXT_SESSION_GEN.fetch_add(1, Ordering::Relaxed);

        let audio1 = Arc::new(AtomicBool::new(true));
        let audio2 = Arc::new(AtomicBool::new(true));
        {
            let mut map = mgr.sessions.lock().unwrap();
            map.insert("dup-sid".into(), SessionHandle { generation: gen1, tx: tx1, audio_enabled: audio1 });
            map.insert("dup-sid".into(), SessionHandle { generation: gen2, tx: tx2, audio_enabled: audio2.clone() });
            // Stale gen1 cleanup MUST NOT remove gen2
            if map.get("dup-sid").is_some_and(|h| h.generation == gen1) {
                map.remove("dup-sid");
            }
            assert_eq!(map.get("dup-sid").map(|h| h.generation), Some(gen2));
        }
        mgr.set_audio_enabled("dup-sid", false);
        assert!(!audio2.load(Ordering::Relaxed));
        mgr.set_audio_enabled("dup-sid", true);
        assert!(audio2.load(Ordering::Relaxed));
        {
            let mut map = mgr.sessions.lock().unwrap();
            // Matching gen2 cleanup removes it cleanly
            if map.get("dup-sid").is_some_and(|h| h.generation == gen2) {
                map.remove("dup-sid");
            }
            assert!(!map.contains_key("dup-sid"));
        }
    }
}
