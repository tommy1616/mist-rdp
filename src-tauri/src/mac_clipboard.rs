#![cfg(target_os = "macos")]

use crate::dib::{bmp_to_dib, dib_to_bmp, fnv1a_64};
use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy, CliprdrBackend};
use ironrdp_cliprdr::pdu::{
    ClipboardFileAttributes, ClipboardFormat, ClipboardFormatId, ClipboardFormatName,
    ClipboardGeneralCapabilityFlags, FileContentsFlags, FileContentsRequest, FileContentsResponse,
    FileDescriptor, FormatDataRequest, FormatDataResponse, LockDataId, OwnedFormatDataResponse,
};
use ironrdp_core::impl_as_any;
use std::collections::HashMap;
use std::ffi::{c_char, c_void};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CF_TEXT: ClipboardFormatId = ClipboardFormatId(1);
const CF_DIB: ClipboardFormatId = ClipboardFormatId(8);
const CF_UNICODETEXT: ClipboardFormatId = ClipboardFormatId(13);
const CF_PNG: ClipboardFormatId = ClipboardFormatId(0xC050);
const PNG_FORMAT_NAME: &str = "PNG";

fn png_clipboard_format() -> ClipboardFormat {
    ClipboardFormat::new(CF_PNG).with_name(ClipboardFormatName::new(PNG_FORMAT_NAME))
}

fn is_png_format(fmt: &ClipboardFormat) -> bool {
    fmt.name
        .as_ref()
        .is_some_and(|n| n.value().eq_ignore_ascii_case("PNG") || n.value().eq_ignore_ascii_case("image/png"))
}
const CLIP_TMP_DIR: &str = "/tmp/mistrdp-cliprdr";
/// Cap image sync to 32 MiB to avoid sluggish conversions on giant bitmaps.
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
/// Cap automatic remote->macOS clipboard file staging at 256 MiB total.
const MAX_REMOTE_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
const MAX_LOCAL_FILES: usize = 2000;
const FILE_CHUNK_SIZE: u32 = 1024 * 1024;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);
static STREAM_SEQ: AtomicU32 = AtomicU32::new(1);

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
}

/// Read `[[NSPasteboard generalPasteboard] changeCount]` in-process without spawning any child process.
/// Returns `None` only if AppKit/ObjC symbols fail unexpectedly.
fn mac_pasteboard_change_count() -> Option<isize> {
    unsafe {
        let cls = objc_getClass(c"NSPasteboard".as_ptr());
        if cls.is_null() {
            return None;
        }
        let sel_gp = sel_registerName(c"generalPasteboard".as_ptr());
        let sel_cc = sel_registerName(c"changeCount".as_ptr());

        let send_ptr: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_isize: unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());

        let pb = send_ptr(cls, sel_gp);
        if pb.is_null() {
            return None;
        }
        Some(send_isize(pb, sel_cc))
    }
}

struct WritingGuard {
    flag: Arc<AtomicBool>,
}

impl WritingGuard {
    fn acquire(flag: &Arc<AtomicBool>) -> Self {
        flag.store(true, Ordering::SeqCst);
        Self { flag: flag.clone() }
    }
}

impl Drop for WritingGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

struct TempFileGuard {
    path: PathBuf,
}

impl TempFileGuard {
    fn new(ext: &str) -> Option<Self> {
        fs::create_dir_all(CLIP_TMP_DIR).ok()?;
        let pid = std::process::id();
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = Path::new(CLIP_TMP_DIR).join(format!("clip-{pid}-{seq}.{ext}"));
        Some(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Read copied Finder file URLs from macOS pasteboard as UTF-8 POSIX paths.
fn read_mac_pasteboard_files() -> Vec<PathBuf> {
    const JXA_READ_FILES: &str = r#"
ObjC.import('AppKit');
var pb = $.NSPasteboard.generalPasteboard;
var urls = pb.readObjectsForClassesOptions($([$.NSURL]), $({NSPasteboardURLReadingFileURLsOnlyKey: true}));
var out = [];
if (urls) {
  var n = urls.count;
  for (var i = 0; i < n; i++) {
    var u = urls.objectAtIndex(i);
    if (u && u.isFileURL) {
      var p = ObjC.unwrap(u.path);
      if (p) out.push(p);
    }
  }
}
out.join('\n');
"#;
    let Ok(output) = Command::new("osascript")
        .args(["-l", "JavaScript", "-e", JXA_READ_FILES])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .collect()
}

/// Put one or more local file/folder paths onto macOS pasteboard so Finder Cmd+V pastes them.
fn write_mac_pasteboard_files(paths: &[PathBuf]) -> bool {
    if paths.is_empty() {
        return false;
    }
    const JXA_WRITE_FILES: &str = r#"
ObjC.import('AppKit');
function run(argv) {
  var urls = [];
  for (var i = 0; i < argv.length; i++) {
    urls.push($.NSURL.fileURLWithPath(argv[i]));
  }
  var pb = $.NSPasteboard.generalPasteboard;
  pb.clearContents();
  return pb.writeObjects($(urls));
}
"#;
    let mut cmd = Command::new("osascript");
    cmd.args(["-l", "JavaScript", "-e", JXA_WRITE_FILES, "--"]);
    for p in paths {
        if let Some(s) = p.to_str() {
            cmd.arg(s);
        }
    }
    cmd.stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[derive(Clone)]
struct LocalMacFile {
    descriptor: FileDescriptor,
    path: PathBuf,
    is_dir: bool,
    size: u64,
}

fn collect_mac_file_entry(
    path: &Path,
    name: &str,
    relative_path: Option<&str>,
    out: &mut Vec<LocalMacFile>,
) {
    if out.len() >= MAX_LOCAL_FILES {
        return;
    }
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    let is_dir = meta.is_dir();
    let attrs = if is_dir {
        ClipboardFileAttributes::DIRECTORY
    } else {
        ClipboardFileAttributes::NORMAL
    };
    let mut desc = FileDescriptor::new(name).with_attributes(attrs);
    if let Some(rel) = relative_path {
        desc = desc.with_relative_path(rel);
    }
    let size = if is_dir { 0 } else { meta.len() };
    if !is_dir {
        desc = desc.with_file_size(size);
    }
    // MS-RDPECLIP limits wire fileName to 259 UTF-16 code units
    let wire_len = match relative_path {
        Some(rel) if !rel.is_empty() => rel.chars().count() + 1 + name.chars().count(),
        _ => name.chars().count(),
    };
    if wire_len > 259 {
        return;
    }

    out.push(LocalMacFile {
        descriptor: desc,
        path: path.to_path_buf(),
        is_dir,
        size,
    });

    if !is_dir {
        return;
    }

    let child_rel = match relative_path {
        Some(rel) => format!("{rel}\\{name}"),
        None => name.to_owned(),
    };
    let Ok(rd) = fs::read_dir(path) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if out.len() >= MAX_LOCAL_FILES {
            break;
        }
        let child_path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_symlink())
            && fs::metadata(&child_path).is_ok_and(|m| m.is_dir())
        {
            continue;
        }
        let fname = entry.file_name();
        if let Some(cname) = fname.to_str() {
            collect_mac_file_entry(&child_path, cname, Some(&child_rel), out);
        }
    }
}

fn build_mac_local_file_list(roots: &[PathBuf]) -> Vec<LocalMacFile> {
    let mut out = Vec::new();
    for root in roots {
        if let Some(name) = root.file_name().and_then(|s| s.to_str()) {
            collect_mac_file_entry(root, name, None, &mut out);
        }
    }
    out
}

fn hash_paths(paths: &[PathBuf]) -> u64 {
    let mut joined = Vec::new();
    for p in paths {
        joined.extend_from_slice(p.to_string_lossy().as_bytes());
        joined.push(0);
    }
    fnv1a_64(&joined)
}

fn read_mac_pasteboard_text() -> Option<String> {
    let output = Command::new("pbpaste").output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn write_mac_pasteboard_text(text: &str) {
    if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

fn mac_clipboard_has_image() -> bool {
    let Ok(output) = Command::new("osascript")
        .args(["-e", "clipboard info"])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let info = String::from_utf8_lossy(&output.stdout);
    info.contains("PNGf")
        || info.contains("TIFF")
        || info.contains("JPEG")
        || info.contains("BMPf")
}

/// Read current macOS clipboard image as PNG bytes (returns None if clipboard has no image).
fn read_mac_pasteboard_png() -> Option<Vec<u8>> {
    if !mac_clipboard_has_image() {
        return None;
    }
    let png_tmp = TempFileGuard::new("png")?;
    let png_path = png_tmp.path().to_str()?;

    let script_png = format!(
        "set fp to open for access (POSIX file \"{png_path}\") with write permission\n\
         try\n\
           set eof of fp to 0\n\
           write (the clipboard as «class PNGf») to fp\n\
           close access fp\n\
         on error\n\
           close access fp\n\
           error \"no png\"\n\
         end try"
    );
    let ok_png = Command::new("osascript")
        .args(["-e", &script_png])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if ok_png {
        let bytes = fs::read(png_tmp.path()).ok()?;
        if !bytes.is_empty() && bytes.len() <= MAX_IMAGE_BYTES {
            return Some(bytes);
        }
    }

    let tiff_tmp = TempFileGuard::new("tiff")?;
    let tiff_path = tiff_tmp.path().to_str()?;
    let script_tiff = format!(
        "set fp to open for access (POSIX file \"{tiff_path}\") with write permission\n\
         try\n\
           set eof of fp to 0\n\
           write (the clipboard as TIFF picture) to fp\n\
           close access fp\n\
         on error\n\
           close access fp\n\
           error \"no tiff\"\n\
         end try"
    );
    let ok_tiff = Command::new("osascript")
        .args(["-e", &script_tiff])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok_tiff {
        return None;
    }

    let conv = Command::new("sips")
        .args(["-s", "format", "png", tiff_path, "--out", png_path])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    if !conv.success() {
        return None;
    }
    let bytes = fs::read(png_tmp.path()).ok()?;
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return None;
    }
    Some(bytes)
}

/// Convert PNG bytes -> Windows CF_DIB bytes via built-in macOS `sips`.
fn png_to_dib(png: &[u8]) -> Option<Vec<u8>> {
    if png.is_empty() || png.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let png_tmp = TempFileGuard::new("png")?;
    let bmp_tmp = TempFileGuard::new("bmp")?;
    fs::write(png_tmp.path(), png).ok()?;

    let status = Command::new("sips")
        .args([
            "-s",
            "format",
            "bmp",
            png_tmp.path().to_str()?,
            "--out",
            bmp_tmp.path().to_str()?,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let bmp = fs::read(bmp_tmp.path()).ok()?;
    if bmp.len() > MAX_IMAGE_BYTES {
        return None;
    }
    bmp_to_dib(&bmp)
}

/// Place raw PNG bytes directly onto macOS clipboard.
fn write_mac_pasteboard_png(
    png_bytes: &[u8],
    shared: &Arc<Mutex<SharedClipboardState>>,
    writing_local: &Arc<AtomicBool>,
) -> bool {
    if png_bytes.is_empty() || png_bytes.len() > MAX_IMAGE_BYTES {
        return false;
    }
    let Some(png_tmp) = TempFileGuard::new("png") else {
        return false;
    };
    if fs::write(png_tmp.path(), png_bytes).is_err() {
        return false;
    }
    let Some(png_path) = png_tmp.path().to_str() else {
        return false;
    };
    let hash = fnv1a_64(png_bytes);

    let _guard = WritingGuard::acquire(writing_local);
    let script = format!("set the clipboard to (read (POSIX file \"{png_path}\") as «class PNGf»)");
    let ok = Command::new("osascript")
        .args(["-e", &script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if ok {
        let new_cc = mac_pasteboard_change_count();
        if let Ok(mut g) = shared.lock() {
            g.last_image_hash = hash;
            g.cached_png = None;
            if let Some(cc) = new_cc {
                g.last_change_count = Some(cc);
            }
        }
    }
    ok
}

/// Convert Windows CF_DIB bytes -> PNG via `sips` and place onto macOS clipboard.
fn write_mac_pasteboard_dib(
    dib: &[u8],
    shared: &Arc<Mutex<SharedClipboardState>>,
    writing_local: &Arc<AtomicBool>,
) -> bool {
    if dib.is_empty() || dib.len() > MAX_IMAGE_BYTES {
        return false;
    }
    let Some(bmp) = dib_to_bmp(dib) else {
        return false;
    };
    let Some(bmp_tmp) = TempFileGuard::new("bmp") else {
        return false;
    };
    let Some(png_tmp) = TempFileGuard::new("png") else {
        return false;
    };
    if fs::write(bmp_tmp.path(), &bmp).is_err() {
        return false;
    }

    let (Some(bmp_path), Some(png_path)) = (bmp_tmp.path().to_str(), png_tmp.path().to_str()) else {
        return false;
    };

    let ok_conv = Command::new("sips")
        .args(["-s", "format", "png", bmp_path, "--out", png_path])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok_conv {
        return false;
    }

    let Ok(png_bytes) = fs::read(png_tmp.path()) else {
        return false;
    };
    if png_bytes.is_empty() {
        return false;
    }
    let hash = fnv1a_64(&png_bytes);

    let _guard = WritingGuard::acquire(writing_local);
    let script = format!("set the clipboard to (read (POSIX file \"{png_path}\") as «class PNGf»)");
    let ok = Command::new("osascript")
        .args(["-e", &script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if ok {
        let new_cc = mac_pasteboard_change_count();
        if let Ok(mut g) = shared.lock() {
            g.last_image_hash = hash;
            g.cached_png = None;
            if let Some(cc) = new_cc {
                g.last_change_count = Some(cc);
            }
        }
    }
    ok
}

pub struct MacClipboardGuard {
    stop: Arc<AtomicBool>,
}

impl Drop for MacClipboardGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct SharedClipboardState {
    last_text: String,
    last_image_hash: u64,
    last_files_hash: u64,
    cached_png: Option<Vec<u8>>,
    local_files: Vec<LocalMacFile>,
    locked_files: HashMap<u32, Vec<LocalMacFile>>,
    last_change_count: Option<isize>,
    channel_ready: bool,
    file_transfer_negotiated: bool,
}

struct QueuedRemoteFile {
    remote_index: i32,
    dest_path: PathBuf,
    expected_size: Option<u64>,
}

struct RemoteDownloadState {
    clip_data_id: Option<u32>,
    top_level_paths: Vec<PathBuf>,
    queue: Vec<QueuedRemoteFile>,
    cursor: usize,
    current_file: Option<File>,
    bytes_written: u64,
    total_written: u64,
    awaiting_size: bool,
    expected_stream_id: u32,
}

pub struct MacClipboardBackend {
    proxy: Box<dyn ClipboardMessageProxy>,
    shared: Arc<Mutex<SharedClipboardState>>,
    writing_local: Arc<AtomicBool>,
    pending_format: Option<ClipboardFormatId>,
    pending_is_png: bool,
    remote_download: Option<RemoteDownloadState>,
}

impl core::fmt::Debug for MacClipboardBackend {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MacClipboardBackend").finish()
    }
}

impl_as_any!(MacClipboardBackend);

pub fn spawn_mac_clipboard(
    proxy: impl ClipboardMessageProxy + Clone + 'static,
) -> (Box<dyn CliprdrBackend>, MacClipboardGuard) {
    let initial_cc = mac_pasteboard_change_count();
    let initial_files = read_mac_pasteboard_files();
    let initial_files_hash = hash_paths(&initial_files);
    let initial_local_files = build_mac_local_file_list(&initial_files);
    let initial_text = read_mac_pasteboard_text().unwrap_or_default();
    let initial_png = if initial_local_files.is_empty() {
        read_mac_pasteboard_png()
    } else {
        None
    };
    let initial_img_hash = initial_png.as_deref().map(fnv1a_64).unwrap_or(0);

    let shared = Arc::new(Mutex::new(SharedClipboardState {
        last_text: initial_text,
        last_image_hash: initial_img_hash,
        last_files_hash: initial_files_hash,
        cached_png: initial_png,
        local_files: initial_local_files,
        locked_files: HashMap::new(),
        last_change_count: initial_cc,
        channel_ready: false,
        file_transfer_negotiated: false,
    }));
    let stop = Arc::new(AtomicBool::new(false));
    let writing_local = Arc::new(AtomicBool::new(false));

    let watcher_shared = shared.clone();
    let watcher_stop = stop.clone();
    let watcher_writing = writing_local.clone();
    let watcher_proxy = proxy.clone();

    std::thread::Builder::new()
        .name("mistrdp-mac-cliprdr".to_owned())
        .spawn(move || {
            while !watcher_stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(450));
                if watcher_stop.load(Ordering::Relaxed) {
                    break;
                }
                if watcher_writing.load(Ordering::SeqCst) {
                    continue;
                }

                // Zero-cost in-process check via NSPasteboard.changeCount
                if let Some(cc) = mac_pasteboard_change_count() {
                    let unchanged = watcher_shared
                        .lock()
                        .ok()
                        .is_some_and(|g| g.last_change_count == Some(cc));
                    if unchanged {
                        continue;
                    }
                    if let Ok(mut g) = watcher_shared.lock() {
                        g.last_change_count = Some(cc);
                    }
                }

                if watcher_writing.load(Ordering::SeqCst) {
                    continue;
                }

                // 1) Check Finder file copies first (Finder also puts filename text on pasteboard)
                let copied_files = read_mac_pasteboard_files();
                if !copied_files.is_empty() {
                    if watcher_writing.load(Ordering::SeqCst) {
                        continue;
                    }
                    let fhash = hash_paths(&copied_files);
                    let entries = build_mac_local_file_list(&copied_files);
                    if !entries.is_empty() {
                        let descriptors: Vec<FileDescriptor> =
                            entries.iter().map(|e| e.descriptor.clone()).collect();
                        let should_send = {
                            let mut guard = watcher_shared
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());
                            let changed = guard.last_files_hash != fhash;
                            guard.last_files_hash = fhash;
                            guard.local_files = entries;
                            guard.cached_png = None;
                            changed && guard.channel_ready && guard.file_transfer_negotiated
                        };
                        if should_send {
                            watcher_proxy.send_clipboard_message(
                                ClipboardMessage::SendInitiateFileCopy(descriptors),
                            );
                        }
                        continue;
                    }
                }

                // 2) Check image changes
                if let Some(png) = read_mac_pasteboard_png() {
                    if watcher_writing.load(Ordering::SeqCst) {
                        continue;
                    }
                    let hash = fnv1a_64(&png);
                    let changed = {
                        let mut guard = watcher_shared
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        if guard.last_image_hash != hash {
                            guard.last_image_hash = hash;
                            guard.cached_png = Some(png);
                            guard.local_files.clear();
                            guard.channel_ready
                        } else {
                            false
                        }
                    };
                    if changed {
                        watcher_proxy.send_clipboard_message(ClipboardMessage::SendInitiateCopy(
                            vec![png_clipboard_format(), ClipboardFormat::new(CF_DIB)],
                        ));
                        continue;
                    }
                }

                // 3) Check text changes
                if let Some(current) = read_mac_pasteboard_text() {
                    if watcher_writing.load(Ordering::SeqCst) {
                        continue;
                    }
                    if !current.is_empty() {
                        let changed = {
                            let mut guard = watcher_shared
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());
                            if guard.last_text != current {
                                guard.last_text = current;
                                guard.cached_png = None;
                                guard.local_files.clear();
                                guard.channel_ready
                            } else {
                                false
                            }
                        };
                        if changed {
                            watcher_proxy.send_clipboard_message(
                                ClipboardMessage::SendInitiateCopy(vec![
                                    ClipboardFormat::new(CF_UNICODETEXT),
                                    ClipboardFormat::new(CF_TEXT),
                                ]),
                            );
                        }
                    }
                }
            }
        })
        .ok();

    let backend = MacClipboardBackend {
        proxy: Box::new(proxy),
        shared,
        writing_local,
        pending_format: None,
        pending_is_png: false,
        remote_download: None,
    };

    (Box::new(backend), MacClipboardGuard { stop })
}

impl MacClipboardBackend {
    fn request_next_download_step(&mut self) {
        let Some(dl) = self.remote_download.as_mut() else {
            return;
        };

        while dl.cursor < dl.queue.len() {
            let item = &dl.queue[dl.cursor];
            if dl.current_file.is_none() {
                if let Some(parent) = item.dest_path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                match File::create(&item.dest_path) {
                    Ok(f) => {
                        dl.current_file = Some(f);
                        dl.bytes_written = 0;
                    }
                    Err(_) => {
                        dl.cursor += 1;
                        continue;
                    }
                }
            }

            // If size unknown, query SIZE first
            if item.expected_size.is_none() {
                let sid = STREAM_SEQ.fetch_add(1, Ordering::Relaxed);
                dl.awaiting_size = true;
                dl.expected_stream_id = sid;
                let req = FileContentsRequest {
                    stream_id: sid,
                    index: item.remote_index,
                    flags: FileContentsFlags::SIZE,
                    position: 0,
                    requested_size: 8,
                    data_id: dl.clip_data_id,
                };
                self.proxy
                    .send_clipboard_message(ClipboardMessage::SendFileContentsRequest(req));
                return;
            }

            let target_size = item.expected_size.unwrap_or(0);
            if dl.bytes_written >= target_size {
                dl.current_file = None;
                dl.bytes_written = 0;
                dl.cursor += 1;
                continue;
            }

            let remaining = target_size - dl.bytes_written;
            if dl.total_written.saturating_add(remaining) > MAX_REMOTE_DOWNLOAD_BYTES {
                // Skip overly large file safely
                dl.current_file = None;
                dl.bytes_written = 0;
                dl.cursor += 1;
                continue;
            }

            let want = remaining.min(u64::from(FILE_CHUNK_SIZE)) as u32;
            let sid = STREAM_SEQ.fetch_add(1, Ordering::Relaxed);
            dl.awaiting_size = false;
            dl.expected_stream_id = sid;
            let req = FileContentsRequest {
                stream_id: sid,
                index: item.remote_index,
                flags: FileContentsFlags::RANGE,
                position: dl.bytes_written,
                requested_size: want,
                data_id: dl.clip_data_id,
            };
            self.proxy
                .send_clipboard_message(ClipboardMessage::SendFileContentsRequest(req));
            return;
        }

        // All queued files finished — publish staged files to macOS pasteboard!
        if let Some(done) = self.remote_download.take() {
            if !done.top_level_paths.is_empty() {
                let _guard = WritingGuard::acquire(&self.writing_local);
                if write_mac_pasteboard_files(&done.top_level_paths) {
                    let fhash = hash_paths(&done.top_level_paths);
                    let new_cc = mac_pasteboard_change_count();
                    if let Ok(mut g) = self.shared.lock() {
                        g.last_files_hash = fhash;
                        g.cached_png = None;
                        if let Some(cc) = new_cc {
                            g.last_change_count = Some(cc);
                        }
                    }
                }
            }
        }
    }
}

impl CliprdrBackend for MacClipboardBackend {
    fn temporary_directory(&self) -> &str {
        CLIP_TMP_DIR
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
            | ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED
            | ClipboardGeneralCapabilityFlags::FILECLIP_NO_FILE_PATHS
            | ClipboardGeneralCapabilityFlags::CAN_LOCK_CLIPDATA
            | ClipboardGeneralCapabilityFlags::HUGE_FILE_SUPPORT_ENABLED
    }

    fn on_ready(&mut self) {
        let pending_files = {
            let mut g = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            g.channel_ready = true;
            if g.file_transfer_negotiated && !g.local_files.is_empty() {
                Some(
                    g.local_files
                        .iter()
                        .map(|e| e.descriptor.clone())
                        .collect::<Vec<_>>(),
                )
            } else {
                None
            }
        };
        if let Some(descriptors) = pending_files {
            self.proxy
                .send_clipboard_message(ClipboardMessage::SendInitiateFileCopy(descriptors));
        }
    }

    fn on_request_format_list(&mut self) {
        let mut formats = Vec::new();
        if let Some(png) = read_mac_pasteboard_png() {
            let hash = fnv1a_64(&png);
            if let Ok(mut g) = self.shared.lock() {
                g.last_image_hash = hash;
                g.cached_png = Some(png);
            }
            formats.push(png_clipboard_format());
            formats.push(ClipboardFormat::new(CF_DIB));
        }
        if let Some(current) = read_mac_pasteboard_text() {
            if !current.is_empty() {
                if let Ok(mut g) = self.shared.lock() {
                    g.last_text = current;
                }
                formats.push(ClipboardFormat::new(CF_UNICODETEXT));
                formats.push(ClipboardFormat::new(CF_TEXT));
            }
        }
        self.proxy
            .send_clipboard_message(ClipboardMessage::SendInitiateCopy(formats));
    }

    fn on_process_negotiated_capabilities(&mut self, capabilities: ClipboardGeneralCapabilityFlags) {
        if let Ok(mut g) = self.shared.lock() {
            g.file_transfer_negotiated =
                capabilities.contains(ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED);
        }
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        self.remote_download = None;
        let can_files = self
            .shared
            .lock()
            .ok()
            .is_some_and(|g| g.file_transfer_negotiated);

        if can_files {
            if let Some(file_fmt) = available_formats
                .iter()
                .find(|f| f.name.as_ref().is_some_and(|n| *n == ClipboardFormatName::FILE_LIST))
            {
                self.pending_format = Some(file_fmt.id);
                self.proxy
                    .send_clipboard_message(ClipboardMessage::SendInitiatePaste(file_fmt.id));
                return;
            }
        }

        self.pending_is_png = false;
        let pick = if available_formats.iter().any(|f| f.id == CF_UNICODETEXT) {
            Some((CF_UNICODETEXT, false))
        } else if let Some(png_fmt) = available_formats.iter().find(|f| is_png_format(f)) {
            Some((png_fmt.id, true))
        } else if available_formats.iter().any(|f| f.id == CF_DIB) {
            Some((CF_DIB, false))
        } else if available_formats.iter().any(|f| f.id == CF_TEXT) {
            Some((CF_TEXT, false))
        } else {
            None
        };
        if let Some((fmt, is_png)) = pick {
            self.pending_format = Some(fmt);
            self.pending_is_png = is_png;
            self.proxy
                .send_clipboard_message(ClipboardMessage::SendInitiatePaste(fmt));
        }
    }

    fn on_remote_file_list(&mut self, files: &[FileDescriptor], clip_data_id: Option<u32>) {
        if files.is_empty() {
            return;
        }
        let pid = std::process::id();
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let stage_root = Path::new(CLIP_TMP_DIR).join(format!("remote-{pid}-{seq}"));
        if fs::create_dir_all(&stage_root).is_err() {
            return;
        }

        let mut top_level_paths: Vec<PathBuf> = Vec::new();
        let mut queue: Vec<QueuedRemoteFile> = Vec::new();

        for (idx, desc) in files.iter().enumerate() {
            let Ok(remote_index) = i32::try_from(idx) else {
                break;
            };
            let rel_path = match desc.relative_path.as_deref() {
                Some(rel) if !rel.is_empty() => {
                    let normalized = rel.replace('\\', "/");
                    stage_root.join(normalized).join(&desc.name)
                }
                _ => stage_root.join(&desc.name),
            };
            let is_top_level = desc
                .relative_path
                .as_deref()
                .map(str::is_empty)
                .unwrap_or(true);
            if is_top_level && !top_level_paths.contains(&rel_path) {
                top_level_paths.push(rel_path.clone());
            }

            let is_dir = desc
                .attributes
                .is_some_and(|a| a.contains(ClipboardFileAttributes::DIRECTORY));
            if is_dir {
                let _ = fs::create_dir_all(&rel_path);
            } else {
                queue.push(QueuedRemoteFile {
                    remote_index,
                    dest_path: rel_path,
                    expected_size: desc.file_size,
                });
            }
        }

        self.remote_download = Some(RemoteDownloadState {
            clip_data_id,
            top_level_paths,
            queue,
            cursor: 0,
            current_file: None,
            bytes_written: 0,
            total_written: 0,
            awaiting_size: false,
            expected_stream_id: 0,
        });
        self.request_next_download_step();
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let resp = if request.format == CF_PNG {
            let png = self
                .shared
                .lock()
                .ok()
                .and_then(|g| g.cached_png.clone())
                .or_else(read_mac_pasteboard_png);
            match png {
                Some(bytes) => OwnedFormatDataResponse::new_data(bytes),
                None => OwnedFormatDataResponse::new_error(),
            }
        } else if request.format == CF_DIB {
            let png = self
                .shared
                .lock()
                .ok()
                .and_then(|g| g.cached_png.clone())
                .or_else(read_mac_pasteboard_png);
            match png.and_then(|p| png_to_dib(&p)) {
                Some(dib) => OwnedFormatDataResponse::new_data(dib),
                None => OwnedFormatDataResponse::new_error(),
            }
        } else if request.format == CF_UNICODETEXT {
            let text = read_mac_pasteboard_text().unwrap_or_default();
            let normalized = text.replace("\r\n", "\n").replace('\n', "\r\n");
            OwnedFormatDataResponse::new_unicode_string(&normalized)
        } else if request.format == CF_TEXT {
            let text = read_mac_pasteboard_text().unwrap_or_default();
            let normalized = text.replace("\r\n", "\n").replace('\n', "\r\n");
            OwnedFormatDataResponse::new_string(&normalized)
        } else {
            OwnedFormatDataResponse::new_error()
        };
        self.proxy
            .send_clipboard_message(ClipboardMessage::SendFormatData(resp));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            return;
        }
        if self.pending_is_png {
            write_mac_pasteboard_png(response.data(), &self.shared, &self.writing_local);
            return;
        }
        if self.pending_format == Some(CF_DIB) {
            write_mac_pasteboard_dib(response.data(), &self.shared, &self.writing_local);
            return;
        }
        let decoded = match self.pending_format {
            Some(CF_UNICODETEXT) => response.to_unicode_string().ok(),
            Some(CF_TEXT) => response.to_string().ok(),
            _ => response
                .to_unicode_string()
                .ok()
                .or_else(|| response.to_string().ok()),
        };
        if let Some(raw) = decoded {
            let clean = raw.replace("\r\n", "\n");
            let _guard = WritingGuard::acquire(&self.writing_local);
            write_mac_pasteboard_text(&clean);
            let new_cc = mac_pasteboard_change_count();
            if let Ok(mut g) = self.shared.lock() {
                g.last_text = clean;
                g.cached_png = None;
                if let Some(cc) = new_cc {
                    g.last_change_count = Some(cc);
                }
            }
        }
    }

    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        let entry_opt = {
            let guard = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            let list = request
                .data_id
                .and_then(|id| guard.locked_files.get(&id))
                .unwrap_or(&guard.local_files);
            usize::try_from(request.index)
                .ok()
                .and_then(|idx| list.get(idx).cloned())
        };

        let resp = match entry_opt {
            Some(entry) if !entry.is_dir => {
                if request.flags.contains(FileContentsFlags::SIZE) {
                    let size = fs::metadata(&entry.path).map(|m| m.len()).unwrap_or(entry.size);
                    FileContentsResponse::new_size_response(request.stream_id, size)
                } else if request.flags.contains(FileContentsFlags::RANGE) {
                    let want = request.requested_size.min(FILE_CHUNK_SIZE) as usize;
                    match File::open(&entry.path).and_then(|mut f| {
                        f.seek(SeekFrom::Start(request.position))?;
                        let mut buf = vec![0u8; want];
                        let n = f.read(&mut buf)?;
                        buf.truncate(n);
                        Ok(buf)
                    }) {
                        Ok(buf) => FileContentsResponse::new_data_response(request.stream_id, buf),
                        Err(_) => FileContentsResponse::new_error(request.stream_id),
                    }
                } else {
                    FileContentsResponse::new_error(request.stream_id)
                }
            }
            _ => FileContentsResponse::new_error(request.stream_id),
        };

        self.proxy
            .send_clipboard_message(ClipboardMessage::SendFileContentsResponse(resp));
    }

    fn on_file_contents_response(&mut self, response: FileContentsResponse<'_>) {
        let Some(dl) = self.remote_download.as_mut() else {
            return;
        };
        if response.stream_id() != dl.expected_stream_id {
            return;
        }
        if response.is_error() {
            dl.current_file = None;
            dl.bytes_written = 0;
            dl.cursor += 1;
            self.request_next_download_step();
            return;
        }

        if dl.awaiting_size {
            if let Ok(size) = response.data_as_size() {
                if let Some(item) = dl.queue.get_mut(dl.cursor) {
                    item.expected_size = Some(size);
                }
            } else {
                dl.current_file = None;
                dl.cursor += 1;
            }
            self.request_next_download_step();
            return;
        }

        let chunk = response.data();
        if chunk.is_empty() {
            dl.current_file = None;
            dl.bytes_written = 0;
            dl.cursor += 1;
            self.request_next_download_step();
            return;
        }

        if let Some(file) = dl.current_file.as_mut() {
            if file.write_all(chunk).is_ok() {
                dl.bytes_written = dl.bytes_written.saturating_add(chunk.len() as u64);
                dl.total_written = dl.total_written.saturating_add(chunk.len() as u64);
            } else {
                dl.current_file = None;
                dl.bytes_written = 0;
                dl.cursor += 1;
            }
        }
        self.request_next_download_step();
    }

    fn on_lock(&mut self, data_id: LockDataId) {
        if let Ok(mut g) = self.shared.lock() {
            let snapshot = g.local_files.clone();
            g.locked_files.insert(data_id.0, snapshot);
        }
    }

    fn on_unlock(&mut self, data_id: LockDataId) {
        if let Ok(mut g) = self.shared.lock() {
            g.locked_files.remove(&data_id.0);
        }
    }

    fn now_ms(&self) -> u64 {
        ironrdp_cliprdr_native::native_now_ms()
    }

    fn elapsed_ms(&self, since: u64) -> u64 {
        self.now_ms().saturating_sub(since)
    }
}
