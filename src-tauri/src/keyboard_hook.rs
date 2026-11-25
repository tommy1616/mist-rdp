use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookKeyEvent {
    pub scancode: u8,
    pub extended: bool,
    pub pressed: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookAction {
    /// Ctrl+Alt+End -> send Ctrl+Alt+Del to remote session (matches native mstsc)
    SendCtrlAltDel,
    /// Ctrl+Alt+Break -> toggle fullscreen mode (matches native mstsc)
    ToggleFullscreen,
    /// Ctrl+Alt+Home -> reveal top floating HUD bar (matches native mstsc connection bar)
    ShowHud,
    /// Remote PrintScreen (Ctrl+Alt+NumpadPlus) or Alt+PrintScreen (Ctrl+Alt+NumpadMinus)
    RemoteScreenshot { active_window_only: bool },
    /// Alt+Home -> remote Start menu (Ctrl+Esc)
    RemoteStartMenu,
    /// Alt+Delete -> remote active window system menu (Alt+Space)
    RemoteWindowMenu,
    /// Alt+Insert -> cycle remote windows in start order (Alt+Esc)
    RemoteCycleOrder,
    /// Alt+PageUp / Alt+PageDown -> switch remote apps (Alt+Tab / Alt+Shift+Tab)
    RemoteSwitchApp { reverse: bool },
    /// Plain Pause key (needs RDP EXTENDED1 0x1D + 0x45 sequence, never NumLock 0x45)
    SendPause,
}

type KeySink = Arc<dyn Fn(&str, HookKeyEvent) + Send + Sync + 'static>;
type ActionSink = Arc<dyn Fn(&str, HookAction) + Send + Sync + 'static>;
type EnabledCallback = Arc<dyn Fn(bool) + Send + Sync + 'static>;

/// Lock-free 256-bit bitset used inside the low-level keyboard hook to ensure
/// every swallowed keydown has a matched keyup without taking any mutex.
pub struct SwallowedKeySet {
    bits: [AtomicU64; 4],
}

impl SwallowedKeySet {
    pub const fn new() -> Self {
        Self {
            bits: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
        }
    }

    #[allow(dead_code)]
    pub fn insert(&self, vk: u8) {
        let word = (vk >> 6) as usize;
        let mask = 1u64 << (vk & 63);
        self.bits[word].fetch_or(mask, Ordering::SeqCst);
    }

    #[allow(dead_code)]
    pub fn take(&self, vk: u8) -> bool {
        let word = (vk >> 6) as usize;
        let mask = 1u64 << (vk & 63);
        (self.bits[word].fetch_and(!mask, Ordering::SeqCst) & mask) != 0
    }

    pub fn clear(&self) {
        for w in &self.bits {
            w.store(0, Ordering::SeqCst);
        }
    }
}

pub struct KeyboardHookState {
    pub enabled: AtomicBool,
    pub focused: AtomicBool,
    pub canvas_focused: AtomicBool,
    pub modal_open: AtomicBool,
    pub has_session: AtomicBool,
    pub target_hwnd: AtomicIsize,
    pub lctrl_held: AtomicBool,
    pub rctrl_held: AtomicBool,
    pub lshift_held: AtomicBool,
    pub rshift_held: AtomicBool,
    pub lalt_held: AtomicBool,
    pub ralt_held: AtomicBool,
    pub lwin_held: AtomicBool,
    pub rwin_held: AtomicBool,
    /// True when we synthesized a remote LAlt down for Alt+PageUp/PageDown while in local-shortcut mode.
    pub synth_remote_alt: AtomicBool,
    pub swallowed: SwallowedKeySet,
    /// Keys swallowed solely for client-side mstsc meta-actions (keyup suppressed without remote release).
    pub suppressed_only: SwallowedKeySet,
    pub active_session: Mutex<Option<String>>,
    pub sink: OnceLock<KeySink>,
    pub action_sink: OnceLock<ActionSink>,
    pub on_enabled_change: OnceLock<EnabledCallback>,
}

impl KeyboardHookState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            enabled: AtomicBool::new(true),
            focused: AtomicBool::new(true),
            canvas_focused: AtomicBool::new(false),
            modal_open: AtomicBool::new(false),
            has_session: AtomicBool::new(false),
            target_hwnd: AtomicIsize::new(0),
            lctrl_held: AtomicBool::new(false),
            rctrl_held: AtomicBool::new(false),
            lshift_held: AtomicBool::new(false),
            rshift_held: AtomicBool::new(false),
            lalt_held: AtomicBool::new(false),
            ralt_held: AtomicBool::new(false),
            lwin_held: AtomicBool::new(false),
            rwin_held: AtomicBool::new(false),
            synth_remote_alt: AtomicBool::new(false),
            swallowed: SwallowedKeySet::new(),
            suppressed_only: SwallowedKeySet::new(),
            active_session: Mutex::new(None),
            sink: OnceLock::new(),
            action_sink: OnceLock::new(),
            on_enabled_change: OnceLock::new(),
        })
    }

    #[allow(dead_code)]
    pub fn set_target_hwnd(&self, hwnd: isize) {
        self.target_hwnd.store(hwnd, Ordering::SeqCst);
    }

    pub fn set_focused(&self, focused: bool) {
        self.focused.store(focused, Ordering::SeqCst);
        if !focused {
            self.reset_held_keys();
        }
    }

    pub fn set_canvas_focused(&self, focused: bool) {
        self.canvas_focused.store(focused, Ordering::SeqCst);
        if !focused {
            self.reset_held_keys();
        }
    }

    pub fn set_modal_open(&self, open: bool) {
        self.modal_open.store(open, Ordering::SeqCst);
        if open {
            self.reset_held_keys();
        }
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::SeqCst);
        self.reset_held_keys();
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    pub fn has_active_session(&self) -> bool {
        self.has_session.load(Ordering::Relaxed)
    }

    pub fn reset_held_keys(&self) {
        self.lctrl_held.store(false, Ordering::SeqCst);
        self.rctrl_held.store(false, Ordering::SeqCst);
        self.lshift_held.store(false, Ordering::SeqCst);
        self.rshift_held.store(false, Ordering::SeqCst);
        self.lalt_held.store(false, Ordering::SeqCst);
        self.ralt_held.store(false, Ordering::SeqCst);
        self.lwin_held.store(false, Ordering::SeqCst);
        self.rwin_held.store(false, Ordering::SeqCst);
        self.synth_remote_alt.store(false, Ordering::SeqCst);
        self.swallowed.clear();
        self.suppressed_only.clear();
    }

    pub fn set_active_session(&self, session_id: Option<String>) {
        self.has_session.store(session_id.is_some(), Ordering::SeqCst);
        if session_id.is_none() {
            self.canvas_focused.store(false, Ordering::SeqCst);
            self.reset_held_keys();
        }
        if let Ok(mut guard) = self.active_session.lock() {
            *guard = session_id;
        }
    }

    pub fn get_active_session(&self) -> Option<String> {
        self.active_session
            .lock()
            .ok()
            .and_then(|g| g.clone())
    }

    pub fn set_sink(&self, sink: KeySink) {
        let _ = self.sink.set(sink);
    }

    pub fn set_action_sink(&self, sink: ActionSink) {
        let _ = self.action_sink.set(sink);
    }

    pub fn set_on_enabled_change(&self, cb: EnabledCallback) {
        let _ = self.on_enabled_change.set(cb);
    }

    #[allow(dead_code)]
    fn notify_enabled_change(&self, enabled: bool) {
        if let Some(cb) = self.on_enabled_change.get() {
            cb(enabled);
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn emit(&self, ev: HookKeyEvent) -> bool {
        let session_id = self.get_active_session();
        if let (Some(sid), Some(sink)) = (session_id, self.sink.get()) {
            sink(&sid, ev);
            true
        } else {
            false
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn emit_action(&self, action: HookAction) -> bool {
        let session_id = self.get_active_session();
        if let (Some(sid), Some(sink)) = (session_id, self.action_sink.get()) {
            sink(&sid, action);
            true
        } else {
            false
        }
    }
}

/// Normalize Windows VK + raw scancode/extended bit to canonical MS-RDPBCGR Fast-Path scancodes.
/// Notably, Windows `KBDLLHOOKSTRUCT` marks `VK_RSHIFT` (0x36) and `VK_NUMLOCK` (0x45) with
/// `LLKHF_EXTENDED`, whereas RDP strictly expects `extended = false` for both.
#[allow(dead_code)]
pub fn normalize_win_rdp_scancode(vk: u16, raw_scancode: u8, raw_extended: bool) -> (u8, bool) {
    match vk {
        0xA0 => (0x2A, false), // VK_LSHIFT
        0xA1 => (0x36, false), // VK_RSHIFT (NEVER extended in RDP)
        0xA2 => (0x1D, false), // VK_LCONTROL
        0xA3 => (0x1D, true),  // VK_RCONTROL
        0xA4 => (0x38, false), // VK_LMENU (Left Alt)
        0xA5 => (0x38, true),  // VK_RMENU (Right Alt)
        0x5B => (0x5B, true),  // VK_LWIN
        0x5C => (0x5C, true),  // VK_RWIN
        0x5D => (0x5D, true),  // VK_APPS (ContextMenu)
        0x2C => (0x37, true),  // VK_SNAPSHOT (PrintScreen)
        0x90 => (0x45, false), // VK_NUMLOCK (NEVER extended in RDP)
        0x6F => (0x35, true),  // VK_DIVIDE (Numpad /)
        0x09 => (0x0F, false), // VK_TAB
        0x1B => (0x01, false), // VK_ESCAPE
        0x20 => (0x39, false), // VK_SPACE
        0x73 => (0x3E, false), // VK_F4
        0xE2 => (0x56, false), // VK_OEM_102 (IntlBackslash)
        _ => (raw_scancode, raw_extended),
    }
}

/// Hardware media/browser/volume keys and lock-LED keys should stay visible to host OS.
#[allow(dead_code)]
pub fn is_host_passthrough_hardware_key(vk: u16, raw_scancode: u8) -> bool {
    // Hardware browser/volume/media/app-launch keys (0xA6..=0xB7), Sleep (0x5F), Fn/vendor (0xFF)
    if (0xA6..=0xB7).contains(&vk) || vk == 0x5F || vk == 0xFF {
        return true;
    }
    // Lock keys (CapsLock 0x14, NumLock 0x90, ScrollLock 0x91): let host toggle physical LED
    // and let frontend JS forward the single scancode press/release cleanly.
    if matches!(vk, 0x14 | 0x90 | 0x91) {
        return true;
    }
    // Ignore unmapped virtual keys with no hardware scancode (except known specials handled above)
    raw_scancode == 0 && !matches!(vk, 0x5B | 0x5C | 0x5D | 0x2C | 0x13 | 0x03)
}

/// In "快捷鍵：本機" mode, keep true local OS window-switching shortcuts (`Win+*`, `Alt+Tab/Esc/F4`,
/// `Ctrl+Esc`) on the host while synchronously forwarding all normal typing & IME keys directly
/// through `WH_KEYBOARD_LL` so local Windows Bopomofo/IME never intercepts or stutters typing.
#[allow(dead_code)]
pub fn should_keep_local_in_host_mode(
    vk: u16,
    alt_down: bool,
    win_down: bool,
    ctrl_down: bool,
) -> bool {
    // Win keys & Win+* combos stay on host
    if matches!(vk, 0x5B | 0x5C) || win_down {
        return true;
    }
    // Alt keys & Alt+* combos (Alt+Tab, Alt+F4, Alt+Esc) stay on host.
    // Explicit mstsc remote-navigation combos (Alt+PageUp/PageDown/Home/Delete/Insert)
    // are intercepted earlier before this predicate runs.
    if matches!(vk, 0x12 | 0xA4 | 0xA5) || alt_down {
        return true;
    }
    // Ctrl+Esc / Ctrl+Shift+Esc stay on host
    if vk == 0x1B && ctrl_down {
        return true;
    }
    false
}

/// Read the current Windows keyboard lock states: `(scroll_lock, num_lock, caps_lock)`.
#[cfg(windows)]
pub fn read_local_lock_states() -> (bool, bool, bool) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, VK_CAPITAL, VK_NUMLOCK, VK_SCROLL,
    };
    unsafe {
        let scroll = (GetKeyState(VK_SCROLL as i32) & 1) != 0;
        let num = (GetKeyState(VK_NUMLOCK as i32) & 1) != 0;
        let caps = (GetKeyState(VK_CAPITAL as i32) & 1) != 0;
        (scroll, num, caps)
    }
}

#[cfg(not(windows))]
#[allow(dead_code)]
pub fn read_local_lock_states() -> (bool, bool, bool) {
    (false, false, false)
}

#[cfg(windows)]
mod platform {
    use super::{
        is_host_passthrough_hardware_key, normalize_win_rdp_scancode,
        should_keep_local_in_host_mode, HookAction, HookKeyEvent, KeyboardHookState,
    };
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, OnceLock};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_ADD, VK_CANCEL, VK_CONTROL, VK_DELETE, VK_END, VK_HOME, VK_INSERT,
        VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_NEXT, VK_PAUSE, VK_PRIOR,
        VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT, VK_SUBTRACT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW,
        GetWindowThreadProcessId, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
        KBDLLHOOKSTRUCT, LLKHF_ALTDOWN, LLKHF_EXTENDED, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
        WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    static HOOK_STATE: OnceLock<Arc<KeyboardHookState>> = OnceLock::new();

    fn is_our_foreground_window(state: &KeyboardHookState) -> bool {
        let fg_hwnd = unsafe { GetForegroundWindow() };
        if fg_hwnd.is_null() {
            return false;
        }
        let target_hwnd = state.target_hwnd.load(Ordering::Relaxed);
        if target_hwnd != 0 && (fg_hwnd as isize) == target_hwnd {
            return true;
        }
        let mut fg_pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(fg_hwnd, &mut fg_pid) };
        fg_pid != 0 && fg_pid == std::process::id()
    }

    fn update_held_modifier(state: &KeyboardHookState, vk: u16, down: bool) {
        match vk {
            VK_LCONTROL => state.lctrl_held.store(down, Ordering::SeqCst),
            VK_RCONTROL => state.rctrl_held.store(down, Ordering::SeqCst),
            VK_LSHIFT => state.lshift_held.store(down, Ordering::SeqCst),
            VK_RSHIFT => state.rshift_held.store(down, Ordering::SeqCst),
            VK_LMENU => state.lalt_held.store(down, Ordering::SeqCst),
            VK_RMENU => state.ralt_held.store(down, Ordering::SeqCst),
            VK_LWIN => state.lwin_held.store(down, Ordering::SeqCst),
            VK_RWIN => state.rwin_held.store(down, Ordering::SeqCst),
            _ => {}
        }
    }

    unsafe extern "system" fn ll_keyboard_proc(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if code >= 0 {
            if let Some(state) = HOOK_STATE.get() {
                let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
                let vk = kb.vkCode as u16;
                let vk_u8 = vk.min(255) as u8;
                let msg = wparam as u32;
                let pressed = matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN);
                let released = matches!(msg, WM_KEYUP | WM_SYSKEYUP);

                if pressed || released {
                    let (scancode, extended) = normalize_win_rdp_scancode(
                        vk,
                        kb.scanCode as u8,
                        (kb.flags & LLKHF_EXTENDED) != 0,
                    );

                    // 1. Suppress keyup for client-side mstsc meta-hotkeys without sending stray keyup to remote
                    if released && state.suppressed_only.take(vk_u8) {
                        return 1;
                    }

                    // 2. Always match every previously swallowed keydown with its keyup
                    if released && state.swallowed.take(vk_u8) {
                        update_held_modifier(state, vk, false);
                        let _ = state.emit(HookKeyEvent {
                            scancode,
                            extended,
                            pressed: false,
                        });
                        return 1;
                    }

                    // 3. If physical Alt is released after a synthesized remote Alt+PageUp/PageDown,
                    // release remote LAlt cleanly so remote Windows completes the Alt+Tab switch!
                    if released
                        && (vk == VK_LMENU || vk == VK_RMENU)
                        && state.synth_remote_alt.swap(false, Ordering::SeqCst)
                    {
                        let _ = state.emit(HookKeyEvent {
                            scancode: 0x38,
                            extended: false,
                            pressed: false,
                        });
                    }

                    let our_fg = is_our_foreground_window(state);
                    let session_fg = our_fg
                        && state.has_active_session()
                        && !state.modal_open.load(Ordering::Relaxed);
                    let canvas_live = session_fg && state.canvas_focused.load(Ordering::Relaxed);

                    if session_fg {
                        let host_alt_down = (kb.flags & LLKHF_ALTDOWN) != 0
                            || (unsafe { GetAsyncKeyState(VK_MENU as i32) } as u16 & 0x8000) != 0;
                        let host_ctrl_down =
                            (unsafe { GetAsyncKeyState(VK_CONTROL as i32) } as u16 & 0x8000) != 0;
                        let host_win_down =
                            (unsafe { GetAsyncKeyState(VK_LWIN as i32) } as u16 & 0x8000) != 0
                                || (unsafe { GetAsyncKeyState(VK_RWIN as i32) } as u16 & 0x8000)
                                    != 0;

                        let alt_down = state.lalt_held.load(Ordering::Relaxed)
                            || state.ralt_held.load(Ordering::Relaxed)
                            || host_alt_down;
                        let ctrl_down = state.lctrl_held.load(Ordering::Relaxed)
                            || state.rctrl_held.load(Ordering::Relaxed)
                            || host_ctrl_down;
                        let win_down = state.lwin_held.load(Ordering::Relaxed)
                            || state.rwin_held.load(Ordering::Relaxed)
                            || host_win_down;

                        // 4. Native Windows mstsc client meta-shortcuts (active in BOTH remote & local modes,
                        // even if focus briefly shifted to the HUD bar).
                        if pressed && ctrl_down && alt_down {
                            let meta_action = match vk {
                                VK_END => Some(HookAction::SendCtrlAltDel),
                                VK_CANCEL | VK_PAUSE => Some(HookAction::ToggleFullscreen),
                                VK_HOME => Some(HookAction::ShowHud),
                                VK_SUBTRACT => {
                                    Some(HookAction::RemoteScreenshot { active_window_only: true })
                                }
                                VK_ADD => Some(HookAction::RemoteScreenshot {
                                    active_window_only: false,
                                }),
                                _ => None,
                            };
                            if let Some(action) = meta_action {
                                state.suppressed_only.insert(vk_u8);
                                let _ = state.emit_action(action);
                                return 1;
                            }
                        }

                        if canvas_live {
                            let hook_enabled = state.is_enabled();
                            let host_shift_down =
                                (unsafe { GetAsyncKeyState(VK_SHIFT as i32) } as u16 & 0x8000) != 0;
                            let _shift_down = state.lshift_held.load(Ordering::Relaxed)
                                || state.rshift_held.load(Ordering::Relaxed)
                                || host_shift_down;

                            // Plain Pause key: send RDP EXTENDED1 0x1D + 0x45 sequence instead of NumLock 0x45
                            if pressed && vk == VK_PAUSE {
                                state.suppressed_only.insert(vk_u8);
                                let _ = state.emit_action(HookAction::SendPause);
                                return 1;
                            }

                            // 5. Native Windows mstsc remote-navigation shortcuts (Alt+PageUp/PageDown/Home/Insert/Delete)
                            // Especially helpful when shortcut toggle is set to "本機" (local Alt+Tab), matching mstsc 1:1.
                            if pressed && alt_down && !ctrl_down {
                                let nav_action = match vk {
                                    VK_PRIOR => Some(HookAction::RemoteSwitchApp { reverse: false }),
                                    VK_NEXT => Some(HookAction::RemoteSwitchApp { reverse: true }),
                                    VK_HOME => Some(HookAction::RemoteStartMenu),
                                    VK_DELETE => Some(HookAction::RemoteWindowMenu),
                                    VK_INSERT => Some(HookAction::RemoteCycleOrder),
                                    _ => None,
                                };
                                if let Some(action) = nav_action {
                                    if matches!(action, HookAction::RemoteSwitchApp { .. })
                                        && !state.lalt_held.load(Ordering::Relaxed)
                                        && !state.ralt_held.load(Ordering::Relaxed)
                                    {
                                        // Synthesize remote LAlt down if local Alt wasn't forwarded, and hold
                                        // until physical Alt is released (handled in step 3 above).
                                        if !state.synth_remote_alt.swap(true, Ordering::SeqCst) {
                                            let _ = state.emit(HookKeyEvent {
                                                scancode: 0x38,
                                                extended: false,
                                                pressed: true,
                                            });
                                        }
                                    }
                                    state.suppressed_only.insert(vk_u8);
                                    let _ = state.emit_action(action);
                                    return 1;
                                }
                            }

                            // Let hardware media/volume/lock-LED keys pass to host cleanly
                            if is_host_passthrough_hardware_key(vk, kb.scanCode as u8) {
                                return unsafe {
                                    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
                                };
                            }

                            // 6. Synchronous low-level forwarding (zero local IME / WebView2 roundtrip!):
                            // - In "快捷鍵：遠端" mode: forward everything (100% native mstsc parity)
                            // - In "快捷鍵：本機" mode: keep only local OS shortcuts (Win+*, Alt+Tab/F4/Esc,
                            //   Ctrl+Esc) on host, while forwarding all typing & IME keys directly to remote.
                            let forward_directly = hook_enabled
                                || !should_keep_local_in_host_mode(vk, alt_down, win_down, ctrl_down);
                            if pressed && forward_directly {
                                update_held_modifier(state, vk, true);
                                state.swallowed.insert(vk_u8);
                                if state.emit(HookKeyEvent {
                                    scancode,
                                    extended,
                                    pressed: true,
                                }) {
                                    return 1;
                                }
                                update_held_modifier(state, vk, false);
                                state.swallowed.take(vk_u8);
                            }
                        }
                    } else if !our_fg {
                        state.lctrl_held.store(false, Ordering::Relaxed);
                        state.rctrl_held.store(false, Ordering::Relaxed);
                        state.lshift_held.store(false, Ordering::Relaxed);
                        state.rshift_held.store(false, Ordering::Relaxed);
                        state.lalt_held.store(false, Ordering::Relaxed);
                        state.ralt_held.store(false, Ordering::Relaxed);
                        state.lwin_held.store(false, Ordering::Relaxed);
                        state.rwin_held.store(false, Ordering::Relaxed);
                    }
                }
            }
        }
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }

    pub fn start(state: Arc<KeyboardHookState>) {
        let _ = HOOK_STATE.set(state);
        std::thread::Builder::new()
            .name("mistrdp-kb-hook".into())
            .spawn(|| unsafe {
                let hmod = GetModuleHandleW(std::ptr::null());
                let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), hmod, 0);
                if hook.is_null() {
                    tracing::warn!("Failed to install WH_KEYBOARD_LL hook");
                    return;
                }
                tracing::info!("Windows WH_KEYBOARD_LL full mstsc-parity hook installed");
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut() as HWND, 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                UnhookWindowsHookEx(hook);
            })
            .ok();
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{HookAction, HookKeyEvent, KeyboardHookState};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicPtr, Ordering};
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;

    static HOOK_STATE: OnceLock<Arc<KeyboardHookState>> = OnceLock::new();
    static TAP_PORT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

    type CGEventTapProxy = *mut c_void;
    type CGEventRef = *mut c_void;
    type CFMachPortRef = *mut c_void;
    type CFRunLoopSourceRef = *mut c_void;
    type CFRunLoopRef = *mut c_void;
    type CFStringRef = *const c_void;

    const K_CG_SESSION_EVENT_TAP: u32 = 1;
    const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
    const K_CG_EVENT_TAP_OPTION_DEFAULT: u32 = 0;
    const K_CG_EVENT_KEY_DOWN: u32 = 10;
    const K_CG_EVENT_KEY_UP: u32 = 11;
    const K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
    const K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;
    const K_CG_KEYBOARD_EVENT_KEYCODE: u32 = 9;
    const K_CG_EVENT_FLAG_MASK_CONTROL: u64 = 0x0004_0000;
    const K_CG_EVENT_FLAG_MASK_ALTERNATE: u64 = 0x0008_0000;
    const K_CG_EVENT_FLAG_MASK_COMMAND: u64 = 0x0010_0000;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            events_of_interest: u64,
            callback: extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef,
            user_info: *mut c_void,
        ) -> CFMachPortRef;
        fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
        fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
        fn CGEventGetFlags(event: CGEventRef) -> u64;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopCommonModes: CFStringRef;
        fn CFMachPortCreateRunLoopSource(
            allocator: *const c_void,
            port: CFMachPortRef,
            order: isize,
        ) -> CFRunLoopSourceRef;
        fn CFRunLoopGetCurrent() -> CFRunLoopRef;
        fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
        fn CFRunLoopRun();
    }

    extern "C" fn tap_callback(
        _proxy: CGEventTapProxy,
        event_type: u32,
        event: CGEventRef,
        _user_info: *mut c_void,
    ) -> CGEventRef {
        if matches!(
            event_type,
            K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT | K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT
        ) {
            let tap = TAP_PORT.load(Ordering::Relaxed);
            if !tap.is_null() {
                unsafe { CGEventTapEnable(tap, true) };
            }
            return event;
        }

        if let Some(state) = HOOK_STATE.get() {
            if state.focused.load(Ordering::Relaxed)
                && state.canvas_focused.load(Ordering::Relaxed)
                && !state.modal_open.load(Ordering::Relaxed)
                && state.has_active_session()
                && matches!(event_type, K_CG_EVENT_KEY_DOWN | K_CG_EVENT_KEY_UP)
            {
                let flags = unsafe { CGEventGetFlags(event) };
                let ctrl_down = (flags & K_CG_EVENT_FLAG_MASK_CONTROL) != 0;
                let opt_down = (flags & K_CG_EVENT_FLAG_MASK_ALTERNATE) != 0;
                let cmd_down = (flags & K_CG_EVENT_FLAG_MASK_COMMAND) != 0;
                let keycode =
                    unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) };
                let pressed = event_type == K_CG_EVENT_KEY_DOWN;

                // Client meta-shortcuts (active in both modes)
                if ctrl_down && opt_down {
                    let action = match keycode {
                        119 => Some(HookAction::SendCtrlAltDel),   // Ctrl+Option+End
                        115 => Some(HookAction::ShowHud),          // Ctrl+Option+Home
                        3 => Some(HookAction::ToggleFullscreen),   // Ctrl+Option+F
                        78 => Some(HookAction::RemoteScreenshot { active_window_only: true }), // Numpad -
                        69 => Some(HookAction::RemoteScreenshot { active_window_only: false }), // Numpad +
                        _ => None,
                    };
                    if let Some(act) = action {
                        if pressed {
                            let _ = state.emit_action(act);
                        }
                        return std::ptr::null_mut();
                    }
                    // Emergency escape: Ctrl+Option+Esc switches to local shortcuts
                    if pressed && keycode == 53 {
                        state.set_enabled(false);
                        state.notify_enabled_change(false);
                        return event;
                    }
                }

                // OS-reserved macOS window/app-switching combos while shortcut forwarding is enabled
                if state.is_enabled() {
                    let mapped = if cmd_down {
                        match keycode {
                            48 => Some((0x0Fu8, false)), // Cmd+Tab
                            49 => Some((0x39u8, false)), // Cmd+Space
                            4 => Some((0x23u8, false)),  // Cmd+H
                            12 => Some((0x10u8, false)), // Cmd+Q
                            46 => Some((0x32u8, false)), // Cmd+M
                            50 => Some((0x29u8, false)), // Cmd+`
                            _ => None,
                        }
                    } else if opt_down && keycode == 48 {
                        Some((0x0Fu8, false)) // Option+Tab (Alt+Tab on remote Windows)
                    } else if ctrl_down {
                        match keycode {
                            49 => Some((0x39u8, false)), // Ctrl+Space (remote IME)
                            123 => Some((0x4Bu8, true)), // Ctrl+Left (Mission Control Space)
                            124 => Some((0x4Du8, true)), // Ctrl+Right
                            125 => Some((0x50u8, true)), // Ctrl+Down
                            126 => Some((0x48u8, true)), // Ctrl+Up
                            _ => None,
                        }
                    } else {
                        None
                    };

                    if let Some((scancode, extended)) = mapped {
                        if state.emit(HookKeyEvent {
                            scancode,
                            extended,
                            pressed,
                        }) {
                            return std::ptr::null_mut();
                        }
                    }
                }
            }
        }
        event
    }

    pub fn start(state: Arc<KeyboardHookState>) {
        let _ = HOOK_STATE.set(state);
        std::thread::Builder::new()
            .name("mistrdp-mac-tap".into())
            .spawn(|| unsafe {
                let mask = (1u64 << K_CG_EVENT_KEY_DOWN) | (1u64 << K_CG_EVENT_KEY_UP);
                let mut warned = false;
                loop {
                    let tap = CGEventTapCreate(
                        K_CG_SESSION_EVENT_TAP,
                        K_CG_HEAD_INSERT_EVENT_TAP,
                        K_CG_EVENT_TAP_OPTION_DEFAULT,
                        mask,
                        tap_callback,
                        std::ptr::null_mut(),
                    );
                    if tap.is_null() {
                        if !warned {
                            tracing::warn!(
                                "macOS CGEventTap waiting for Accessibility permission (grant in System Settings -> Privacy & Security -> Accessibility)"
                            );
                            warned = true;
                        }
                        std::thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                    TAP_PORT.store(tap, Ordering::SeqCst);
                    let src = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
                    let rl = CFRunLoopGetCurrent();
                    CFRunLoopAddSource(rl, src, kCFRunLoopCommonModes);
                    CGEventTapEnable(tap, true);
                    tracing::info!("macOS CGEventTap shortcut hook installed");
                    CFRunLoopRun();
                    break;
                }
            })
            .ok();
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use super::KeyboardHookState;
    use std::sync::Arc;
    pub fn start(_state: Arc<KeyboardHookState>) {}
}

pub fn start_hook(state: Arc<KeyboardHookState>) {
    platform::start(state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rshift_and_special_keys() {
        assert_eq!(normalize_win_rdp_scancode(0xA1, 0x36, true), (0x36, false));
        assert_eq!(normalize_win_rdp_scancode(0xA0, 0x2A, false), (0x2A, false));
        assert_eq!(normalize_win_rdp_scancode(0x90, 0x45, true), (0x45, false));
        assert_eq!(normalize_win_rdp_scancode(0xA4, 0x38, false), (0x38, false));
        assert_eq!(normalize_win_rdp_scancode(0xA5, 0x38, true), (0x38, true));
        assert_eq!(normalize_win_rdp_scancode(0x5B, 0x5B, true), (0x5B, true));
        assert_eq!(normalize_win_rdp_scancode(0x6F, 0x35, false), (0x35, true));
        assert_eq!(normalize_win_rdp_scancode(0xE2, 0x56, false), (0x56, false));
    }

    #[test]
    fn host_passthrough_detects_media_and_lock_keys() {
        assert!(is_host_passthrough_hardware_key(0xAF, 0x30)); // VolumeUp
        assert!(is_host_passthrough_hardware_key(0x14, 0x3A)); // CapsLock
        assert!(is_host_passthrough_hardware_key(0x90, 0x45)); // NumLock
        assert!(is_host_passthrough_hardware_key(0x91, 0x46)); // ScrollLock
        assert!(!is_host_passthrough_hardware_key(0xA1, 0x36)); // Right Shift
        assert!(!is_host_passthrough_hardware_key(0x41, 0x1E)); // KeyA
    }

    #[test]
    fn host_mode_keeps_os_shortcuts_local_and_forwards_typing_keys() {
        // OS shortcuts stay local
        assert!(should_keep_local_in_host_mode(0x5B, false, false, false)); // LWin
        assert!(should_keep_local_in_host_mode(0x44, false, true, false)); // Win+D
        assert!(should_keep_local_in_host_mode(0xA4, false, false, false)); // LAlt
        assert!(should_keep_local_in_host_mode(0x09, true, false, false)); // Alt+Tab
        assert!(should_keep_local_in_host_mode(0x73, true, false, false)); // Alt+F4
        assert!(should_keep_local_in_host_mode(0x1B, false, false, true)); // Ctrl+Esc

        // Typing & remote IME keys bypass local IME directly to remote
        assert!(!should_keep_local_in_host_mode(0x41, false, false, false)); // KeyA (Bopomofo)
        assert!(!should_keep_local_in_host_mode(0x31, false, false, false)); // Digit1 (ㄅ)
        assert!(!should_keep_local_in_host_mode(0x20, false, false, false)); // Space
        assert!(!should_keep_local_in_host_mode(0x20, false, false, true)); // Ctrl+Space
        assert!(!should_keep_local_in_host_mode(0xA1, false, false, false)); // Right Shift
        assert!(!should_keep_local_in_host_mode(0x43, false, false, true)); // Ctrl+C
    }

    #[test]
    fn swallowed_key_set_tracks_and_clears() {
        let set = SwallowedKeySet::new();
        assert!(!set.take(0xA1));
        set.insert(0xA1);
        set.insert(0x09);
        assert!(set.take(0xA1));
        assert!(!set.take(0xA1));
        assert!(set.take(0x09));
        set.insert(0xA4);
        set.clear();
        assert!(!set.take(0xA4));
    }
}
