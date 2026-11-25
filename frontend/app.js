const invoke = window.__TAURI__?.core?.invoke;

const RESOLUTION_PRESETS = [
  { value: 'auto', label: '自動', w: 0, h: 0 },
  { value: '1280x720', label: '1280 × 720', w: 1280, h: 720 },
  { value: '1366x768', label: '1366 × 768', w: 1366, h: 768 },
  { value: '1600x900', label: '1600 × 900', w: 1600, h: 900 },
  { value: '1920x1080', label: '1920 × 1080', w: 1920, h: 1080 },
  { value: '2560x1440', label: '2560 × 1440', w: 2560, h: 1440 },
  { value: '2560x1600', label: '2560 × 1600', w: 2560, h: 1600 },
  { value: '3840x2160', label: '3840 × 2160', w: 3840, h: 2160 },
];

const state = {
  platform: 'windows',
  wsPort: 0,
  wsToken: '',
  ws: null,
  vaultStatus: { exists: false, unlocked: false, vault_path: '', remember_keychain: false },
  data: {
    hosts: [],
    settings: { mac_cmd_to_ctrl: true, accept_self_signed_tls: true, lossless_quality: true },
    vault_path: '',
    remember_keychain: false
  },
  sharePresets: [],
  syncPresets: [],
  syncConfig: {
    mode: 'disabled',
    folder_path: '',
    webdav_url: '',
    webdav_username: '',
    webdav_file_name: 'mist-rdp.vault.json',
    webdav_allow_invalid_certs: false,
    has_webdav_password: false,
    auto_sync: true,
    last_sync_at: null,
    last_sync_status: ''
  },
  syncAuto: true,
  syncDavInsecure: false,
  syncInFlight: false,
  activeNav: 'hosts',
  activeGroup: null,
  tabs: [{ id: 'home', type: 'home', title: '主機' }],
  activeTabId: 'home',
  sessions: new Map(),
  pendingPrompt: null,
  gateRemember: false,
  hostSavePwd: true,
  hostClip: true,
  hostHook: true,
  hostAudio: true,
  promptUpdatePwd: true,
  isMaximized: false,
  isFullscreen: false,
  hudPinned: localStorage.getItem('mistrdp.hudPinned') === '1',
  hudHovering: false,
  hudFocusInside: false
};

// Complete PS/2 Set-1 scancode map [scancode, extended]
const SCANCODE_MAP = {
  Escape: [0x01, false],
  Digit1: [0x02, false], Digit2: [0x03, false], Digit3: [0x04, false], Digit4: [0x05, false],
  Digit5: [0x06, false], Digit6: [0x07, false], Digit7: [0x08, false], Digit8: [0x09, false],
  Digit9: [0x0a, false], Digit0: [0x0b, false], Minus: [0x0c, false], Equal: [0x0d, false],
  Backspace: [0x0e, false], Tab: [0x0f, false],
  KeyQ: [0x10, false], KeyW: [0x11, false], KeyE: [0x12, false], KeyR: [0x13, false],
  KeyT: [0x14, false], KeyY: [0x15, false], KeyU: [0x16, false], KeyI: [0x17, false],
  KeyO: [0x18, false], KeyP: [0x19, false], BracketLeft: [0x1a, false], BracketRight: [0x1b, false],
  Enter: [0x1c, false], ControlLeft: [0x1d, false],
  KeyA: [0x1e, false], KeyS: [0x1f, false], KeyD: [0x20, false], KeyF: [0x21, false],
  KeyG: [0x22, false], KeyH: [0x23, false], KeyJ: [0x24, false], KeyK: [0x25, false],
  KeyL: [0x26, false], Semicolon: [0x27, false], Quote: [0x28, false], Backquote: [0x29, false],
  ShiftLeft: [0x2a, false], Backslash: [0x2b, false],
  KeyZ: [0x2c, false], KeyX: [0x2d, false], KeyC: [0x2e, false], KeyV: [0x2f, false],
  KeyB: [0x30, false], KeyN: [0x31, false], KeyM: [0x32, false], Comma: [0x33, false],
  Period: [0x34, false], Slash: [0x35, false], ShiftRight: [0x36, false],
  NumpadMultiply: [0x37, false], AltLeft: [0x38, false], Space: [0x39, false], CapsLock: [0x3a, false],
  F1: [0x3b, false], F2: [0x3c, false], F3: [0x3d, false], F4: [0x3e, false],
  F5: [0x3f, false], F6: [0x40, false], F7: [0x41, false], F8: [0x42, false],
  F9: [0x43, false], F10: [0x44, false], NumLock: [0x45, false], ScrollLock: [0x46, false],
  Numpad7: [0x47, false], Numpad8: [0x48, false], Numpad9: [0x49, false], NumpadSubtract: [0x4a, false],
  Numpad4: [0x4b, false], Numpad5: [0x4c, false], Numpad6: [0x4d, false], NumpadAdd: [0x4e, false],
  Numpad1: [0x4f, false], Numpad2: [0x50, false], Numpad3: [0x51, false], Numpad0: [0x52, false],
  NumpadDecimal: [0x53, false], IntlBackslash: [0x56, false], F11: [0x57, false], F12: [0x58, false],
  NumpadEnter: [0x1c, true], ControlRight: [0x1d, true], NumpadDivide: [0x35, true],
  PrintScreen: [0x37, true], AltRight: [0x38, true],
  Home: [0x47, true], ArrowUp: [0x48, true], PageUp: [0x49, true],
  ArrowLeft: [0x4b, true], ArrowRight: [0x4d, true],
  End: [0x4f, true], ArrowDown: [0x50, true], PageDown: [0x51, true],
  Insert: [0x52, true], Delete: [0x53, true],
  MetaLeft: [0x5b, true], MetaRight: [0x5c, true], ContextMenu: [0x5d, true]
};

const $ = (id) => document.getElementById(id);

let toastTimer;
function notify(msg, danger = false) {
  const el = $('toast');
  el.textContent = msg;
  el.classList.toggle('danger', danger);
  el.classList.add('show');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove('show'), danger ? 3800 : 2200);
}

function populateResolutionSelects() {
  const baseOpts = RESOLUTION_PRESETS.map(
    (p) => `<option value="${p.value}">${p.label}</option>`
  ).join('');
  $('quickRes').innerHTML = baseOpts;
  $('hudResSelect').innerHTML = baseOpts;
  $('hostResPreset').innerHTML = baseOpts + `<option value="custom">自訂</option>`;
}

function populateSharePresets() {
  const opts = [`<option value="">不共享</option>`];
  for (const p of state.sharePresets) {
    opts.push(`<option value="${escapeHtml(p.path)}">${escapeHtml(p.label)}</option>`);
  }
  opts.push(`<option value="__custom__">自訂路徑</option>`);
  $('hostSharePreset').innerHTML = opts.join('');
}

function calcAutoResolution() {
  const deck = $('canvasDeck');
  const dpr = window.devicePixelRatio || 1;
  const rawW = deck.clientWidth || window.innerWidth;
  const rawH = deck.clientHeight || (window.innerHeight - (state.isFullscreen ? 0 : 44));
  const w = Math.max(800, Math.min(4096, Math.floor(rawW * dpr))) & ~1;
  const h = Math.max(600, Math.min(2160, Math.floor(rawH * dpr))) & ~1;
  return [w, h];
}

function resolveRequestedSize(w, h) {
  if (w >= 640 && h >= 480) {
    return [Math.min(4096, w) & ~1, Math.min(2160, h) & ~1];
  }
  return calcAutoResolution();
}

function resolutionLabel(w, h) {
  return w >= 640 && h >= 480 ? `${w}×${h}` : '自動';
}

function presetForSize(w, h) {
  if (!w || !h) return 'auto';
  const match = RESOLUTION_PRESETS.find((p) => p.w === w && p.h === h);
  return match ? match.value : 'custom';
}

// ── Anti-stuck modifier release ───────────────────────────────────────────────

function releaseSessionModifiers(sid) {
  if (!sid || sid === 'home') return;
  if (state.sessions.has(sid)) {
    invoke?.('rdp_release_modifiers', { sessionId: sid });
  }
}

function releaseActiveSessionModifiers() {
  releaseSessionModifiers(state.activeTabId);
}

function setModalOpenFlag(open) {
  invoke?.('set_modal_open', { open: Boolean(open) });
}

function syncHookToggleUi(sess) {
  const titleBtn = $('btnTitlebarHook');
  const titleLabel = $('titlebarHookLabel');
  if (!sess) {
    titleBtn?.classList.add('hidden');
    return;
  }
  const on = Boolean(sess.hookEnabled);
  titleBtn?.classList.remove('hidden');
  titleBtn?.classList.toggle('on', on);
  if (titleLabel) titleLabel.textContent = on ? '快捷鍵：遠端' : '快捷鍵：本機';
  if (titleBtn) {
    titleBtn.title = on
      ? '系統快捷鍵（Alt+Tab / Win）轉送至遠端，點擊改為本機切換'
      : '系統快捷鍵留在本機（Alt+Tab 切換本機視窗），點擊改為轉送遠端';
  }
}

const AUDIO_SVG_ON = '<polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/><path d="M15.54 8.46a5 5 0 0 1 0 7.07"/>';
const AUDIO_SVG_OFF = '<polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/><line x1="23" y1="9" x2="17" y2="15"/><line x1="17" y1="9" x2="23" y2="15"/>';

function syncLosslessToggleUi(sess) {
  const btn = $('btnHudLossless');
  if (!btn || !sess) return;
  const on = Boolean(sess.losslessQuality);
  btn.classList.toggle('on', on);
  btn.title = on
    ? '無損 32-bit 清晰畫質，點擊切換為 RemoteFX 省頻寬模式'
    : '目前為 RemoteFX 省頻寬模式，點擊切回無損 32-bit 清晰畫質';
}

function syncAudioToggleUi(sess) {
  const btn = $('btnHudAudio');
  const icon = $('hudAudioIcon');
  const text = $('hudAudioText');
  if (!btn || !sess) return;
  const on = Boolean(sess.audioEnabled);
  btn.classList.toggle('on', on);
  btn.title = on ? '聲音已開啟，點擊靜音' : '聲音已靜音，點擊開啟';
  if (icon) icon.innerHTML = on ? AUDIO_SVG_ON : AUDIO_SVG_OFF;
  if (text) text.textContent = on ? '聲音' : '靜音';
}

async function toggleActiveLosslessMode() {
  const sess = state.sessions.get(state.activeTabId);
  if (!sess) return;
  sess.losslessQuality = !sess.losslessQuality;
  syncLosslessToggleUi(sess);

  // Persist as default preference in vault settings
  if (state.vaultStatus.unlocked) {
    try {
      const next = { ...state.data.settings, lossless_quality: sess.losslessQuality };
      const data = await invoke('vault_update_settings', { settings: next });
      applyVaultData(data);
      scheduleAutoSync();
    } catch (_) {}
  }

  notify(sess.losslessQuality ? '已開啟無損畫質，重新連線中' : '已切換為省頻寬模式，重新連線中');
  await reconnectSessionInPlace(sess);
}

function toggleActiveAudio() {
  const sess = state.sessions.get(state.activeTabId);
  if (!sess) return;
  sess.audioEnabled = !sess.audioEnabled;
  invoke?.('rdp_set_audio_enabled', { sessionId: sess.id, enabled: sess.audioEnabled });
  syncAudioToggleUi(sess);
  notify(sess.audioEnabled ? '遠端聲音已開啟' : '遠端聲音已靜音');
  setTimeout(() => sess.canvas?.focus(), 10);
}

function toggleActiveHookMode() {
  const sess = state.sessions.get(state.activeTabId);
  if (!sess) return;
  sess.hookEnabled = !sess.hookEnabled;
  invoke?.('set_keyboard_hook', { enabled: sess.hookEnabled });
  syncHookToggleUi(sess);
  notify(sess.hookEnabled ? '快捷鍵已改為轉送遠端' : '快捷鍵已改為留在本機');
  setTimeout(() => sess.canvas?.focus(), 10);
}

// ── HUD Auto-Hide & Pin ───────────────────────────────────────────────────────

let hudHideTimer = null;

function shouldAllowHudCollapse() {
  if (state.hudPinned || state.hudHovering || state.hudFocusInside) return false;
  if (state.activeTabId === 'home') return false;
  const sess = state.sessions.get(state.activeTabId);
  return Boolean(sess && sess.state === 'connected');
}

function showHud() {
  clearTimeout(hudHideTimer);
  $('rdpHudZone').classList.remove('collapsed');
}

function scheduleHudCollapse(delayMs = 850) {
  clearTimeout(hudHideTimer);
  if (!shouldAllowHudCollapse()) {
    $('rdpHudZone').classList.remove('collapsed');
    return;
  }
  hudHideTimer = setTimeout(() => {
    if (shouldAllowHudCollapse()) {
      $('rdpHudZone').classList.add('collapsed');
    }
  }, delayMs);
}

function syncHudPinUi() {
  $('btnHudPin').classList.toggle('on', state.hudPinned);
  $('btnHudPin').title = state.hudPinned ? '已釘選工具列（點擊改為自動隱藏）' : '自動隱藏工具列（點擊釘選）';
  if (state.hudPinned) {
    showHud();
  } else {
    scheduleHudCollapse(500);
  }
}

function wireHudAutoHide() {
  const zone = $('rdpHudZone');
  const handle = $('hudHandle');

  zone.addEventListener('mouseenter', () => {
    state.hudHovering = true;
    showHud();
  });
  zone.addEventListener('mouseleave', () => {
    state.hudHovering = false;
    scheduleHudCollapse(800);
  });
  handle.addEventListener('mouseenter', () => {
    state.hudHovering = true;
    showHud();
  });
  handle.addEventListener('click', () => {
    showHud();
  });

  // Also reveal when cursor touches the top 8px edge of the RDP stage
  $('viewRdp').addEventListener('mousemove', (e) => {
    if (e.clientY <= (state.isFullscreen ? 8 : 52) && zone.classList.contains('collapsed')) {
      showHud();
      scheduleHudCollapse(1600);
    }
  });

  const resSelect = $('hudResSelect');
  resSelect.addEventListener('focus', () => {
    state.hudFocusInside = true;
    showHud();
  });
  resSelect.addEventListener('blur', () => {
    state.hudFocusInside = false;
    scheduleHudCollapse(700);
  });
}

// ── Dynamic Auto-Resize Observer ──────────────────────────────────────────────

let autoResizeTimer = null;

// Ask the server to match the visible area when the active session is set to "自動".
// Safe to call at any time: it does nothing when the remote size already matches.
function applyAutoResize() {
  if (state.activeTabId === 'home') return;
  const sess = state.sessions.get(state.activeTabId);
  if (!sess || sess.state !== 'connected') return;
  // Only auto-resize sessions configured to "自動" (requestedW === 0 && requestedH === 0)
  if (sess.requestedW !== 0 || sess.requestedH !== 0) return;

  const [w, h] = calcAutoResolution();
  if (Math.abs(w - sess.fbW) <= 4 && Math.abs(h - sess.fbH) <= 4) return;
  // Do not repeat the same request in a burst, but do repeat it later: the remote size still
  // differing means the earlier request never took effect.
  const last = sess.lastAutoResize;
  if (last && last[0] === w && last[1] === h && Date.now() - last[2] < 3000) return;

  sess.lastAutoResize = [w, h, Date.now()];
  invoke?.('rdp_resize', { sessionId: sess.id, width: w, height: h });
}

function scheduleAutoResize(delay = 350) {
  clearTimeout(autoResizeTimer);
  autoResizeTimer = setTimeout(applyAutoResize, delay);
}

function setupAutoResizeObserver() {
  const deck = $('canvasDeck');
  if (typeof ResizeObserver === 'undefined' || !deck) return;

  const observer = new ResizeObserver(() => scheduleAutoResize());
  observer.observe(deck);
}

// ── Bootstrap & WebSocket Framebuffer Stream ──────────────────────────────────

async function bootstrap() {
  populateResolutionSelects();
  wireWindowControls();
  wireUiEvents();
  wireHudAutoHide();
  wireFullscreenSync();
  setupAutoResizeObserver();
  syncHudPinUi();

  window.addEventListener('blur', releaseActiveSessionModifiers);
  window.__TAURI__?.event?.listen?.('hook-state-changed', (ev) => {
    const enabled = Boolean(ev.payload);
    const sess = state.sessions.get(state.activeTabId);
    if (sess) {
      sess.hookEnabled = enabled;
      syncHookToggleUi(sess);
      notify(enabled ? '快捷鍵已改為轉送遠端' : '已切回本機快捷鍵');
    }
  });
  window.__TAURI__?.event?.listen?.('show-hud', () => {
    if (state.activeTabId !== 'home') {
      showHud();
      scheduleHudCollapse(2600);
    }
  });
  window.__TAURI__?.event?.listen?.('window-focused', () => {
    if (state.activeTabId === 'home') return;
    if (document.querySelector('.modal.open')) return;
    const sess = state.sessions.get(state.activeTabId);
    if (sess) {
      sess.locksSynced = false;
      setTimeout(() => sess.canvas?.focus(), 10);
    }
  });
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) releaseActiveSessionModifiers();
  });

  if (!invoke) return;

  const [info, presets, syncPresets, syncCfg] = await Promise.all([
    invoke('get_bootstrap_info'),
    invoke('get_default_share_folders').catch(() => []),
    invoke('get_default_sync_folder_presets').catch(() => []),
    invoke('sync_get_config').catch(() => null)
  ]);
  state.platform = info.platform;
  state.wsPort = info.ws_port;
  state.wsToken = info.ws_token;
  state.vaultStatus = info.vault_status;
  state.sharePresets = Array.isArray(presets) ? presets : [];
  state.syncPresets = Array.isArray(syncPresets) ? syncPresets : [];
  if (syncCfg) state.syncConfig = syncCfg;
  populateSharePresets();
  populateSyncFolderPresets();
  renderSyncConfig();

  connectStreamWs();
  renderTabs();

  // Attempt seamless auto-unlock from OS keychain on startup (never after manual lock)
  if (state.vaultStatus.exists && !state.vaultStatus.unlocked) {
    try {
      const autoData = await invoke('vault_try_auto_unlock');
      if (autoData) {
        applyVaultData(autoData);
        maybeAutoSyncOnUnlock();
        return;
      }
    } catch (_) {}
  }

  renderVaultGate();
}

const wsTextDecoder = new TextDecoder();
const POINTER_CACHE_MAX = 64;
const pointerCache = new Map();
let pointerCanvas = null;
let pointerCtx = null;
let pointerScaleCanvas = null;
let pointerScaleCtx = null;

function hashPointerBitmap(w, h, hotX, hotY, rgba) {
  let hash = 0x811c9dc5;
  const mix = (v) => {
    hash ^= v & 0xff;
    hash = Math.imul(hash, 0x01000193);
  };
  mix(w); mix(w >> 8);
  mix(h); mix(h >> 8);
  mix(hotX); mix(hotX >> 8);
  mix(hotY); mix(hotY >> 8);
  for (let i = 0; i < rgba.length; i++) {
    hash ^= rgba[i];
    hash = Math.imul(hash, 0x01000193);
  }
  return `${w}x${h}@${hotX},${hotY}:${(hash >>> 0).toString(16)}`;
}

function buildPointerCssCursor(w, h, hotX, hotY, rgba) {
  const key = hashPointerBitmap(w, h, hotX, hotY, rgba);
  const cached = pointerCache.get(key);
  if (cached) {
    pointerCache.delete(key);
    pointerCache.set(key, cached);
    return cached;
  }

  if (!pointerCanvas) {
    pointerCanvas = document.createElement('canvas');
    pointerCtx = pointerCanvas.getContext('2d');
  }
  if (pointerCanvas.width !== w || pointerCanvas.height !== h) {
    pointerCanvas.width = w;
    pointerCanvas.height = h;
  }
  const clamped = new Uint8ClampedArray(rgba);
  pointerCtx.putImageData(new ImageData(clamped, w, h), 0, 0);

  let targetCanvas = pointerCanvas;
  let outHotX = Math.max(0, Math.min(w - 1, hotX));
  let outHotY = Math.max(0, Math.min(h - 1, hotY));

  if (w > 128 || h > 128) {
    const scale = Math.min(128 / w, 128 / h);
    const outW = Math.max(1, Math.round(w * scale));
    const outH = Math.max(1, Math.round(h * scale));
    outHotX = Math.max(0, Math.min(outW - 1, Math.round(outHotX * scale)));
    outHotY = Math.max(0, Math.min(outH - 1, Math.round(outHotY * scale)));
    if (!pointerScaleCanvas) {
      pointerScaleCanvas = document.createElement('canvas');
      pointerScaleCtx = pointerScaleCanvas.getContext('2d');
    }
    pointerScaleCanvas.width = outW;
    pointerScaleCanvas.height = outH;
    pointerScaleCtx.clearRect(0, 0, outW, outH);
    pointerScaleCtx.drawImage(pointerCanvas, 0, 0, outW, outH);
    targetCanvas = pointerScaleCanvas;
  }

  const dataUrl = targetCanvas.toDataURL('image/png');
  const css = `url("${dataUrl}") ${outHotX} ${outHotY}, default`;
  pointerCache.set(key, css);
  if (pointerCache.size > POINTER_CACHE_MAX) {
    pointerCache.delete(pointerCache.keys().next().value);
  }
  return css;
}

function applySessionCursor(sess, cssCursor) {
  if (sess.lastCursor === cssCursor) return;
  sess.lastCursor = cssCursor;
  sess.canvas.style.cursor = cssCursor;
}

function markDirtyRect(sess, x, y, w, h) {
  if (w <= 0 || h <= 0) return;
  const x0 = Math.max(0, x);
  const y0 = Math.max(0, y);
  const x1 = Math.min(sess.fbW, x + w);
  const y1 = Math.min(sess.fbH, y + h);
  if (x1 <= x0 || y1 <= y0) return;
  if (!sess.dirty) {
    sess.dirty = true;
    sess.dirtyMinX = x0;
    sess.dirtyMinY = y0;
    sess.dirtyMaxX = x1;
    sess.dirtyMaxY = y1;
  } else {
    if (x0 < sess.dirtyMinX) sess.dirtyMinX = x0;
    if (y0 < sess.dirtyMinY) sess.dirtyMinY = y0;
    if (x1 > sess.dirtyMaxX) sess.dirtyMaxX = x1;
    if (y1 > sess.dirtyMaxY) sess.dirtyMaxY = y1;
  }
}

function connectStreamWs() {
  const url = `ws://127.0.0.1:${state.wsPort}/?token=${state.wsToken}`;
  const ws = new WebSocket(url);
  ws.binaryType = 'arraybuffer';
  state.ws = ws;

  ws.onmessage = (ev) => {
    const buf = ev.data;
    if (!(buf instanceof ArrayBuffer) || buf.byteLength < 1) return;
    const view = new DataView(buf);
    const kind = view.getUint8(0);

    if (kind === 0x01 || kind === 0x02) {
      const sidLen = view.getUint16(1, true);
      const sidBytes = new Uint8Array(buf, 3, sidLen);
      const sid = wsTextDecoder.decode(sidBytes);
      let off = 3 + sidLen;
      const fbW = view.getUint16(off, true); off += 2;
      const fbH = view.getUint16(off, true); off += 2;

      const sess = state.sessions.get(sid);
      if (!sess) return;
      if (sess.backCanvas.width !== fbW || sess.backCanvas.height !== fbH) {
        sess.backCanvas.width = fbW;
        sess.backCanvas.height = fbH;
        sess.canvas.width = fbW;
        sess.canvas.height = fbH;
        sess.fbW = fbW;
        sess.fbH = fbH;
        markDirtyRect(sess, 0, 0, fbW, fbH);
      }

      const rectCount = kind === 0x02 ? view.getUint16(off, true) : 1;
      if (kind === 0x02) off += 2;

      let updated = false;
      for (let i = 0; i < rectCount; i++) {
        if (off + 8 > buf.byteLength) break;
        const left = view.getUint16(off, true); off += 2;
        const top = view.getUint16(off, true); off += 2;
        const rw = view.getUint16(off, true); off += 2;
        const rh = view.getUint16(off, true); off += 2;
        const expected = rw * rh * 4;
        if (rw > 0 && rh > 0 && off + expected <= buf.byteLength) {
          const rgba = new Uint8ClampedArray(buf, off, expected);
          const img = new ImageData(rgba, rw, rh);
          sess.backCtx.putImageData(img, left, top);
          markDirtyRect(sess, left, top, rw, rh);
          updated = true;
        }
        off += expected;
      }
      if (updated) {
        schedulePresent(sess);
      }
    } else if (kind === 0x03) {
      const jsonBytes = new Uint8Array(buf, 1);
      try {
        const payload = JSON.parse(wsTextDecoder.decode(jsonBytes));
        handleSessionStatus(payload);
      } catch (_) {}
    } else if (kind === 0x04) {
      if (buf.byteLength < 4) return;
      const sidLen = view.getUint16(1, true);
      if (3 + sidLen + 1 > buf.byteLength) return;
      const sidBytes = new Uint8Array(buf, 3, sidLen);
      const sid = wsTextDecoder.decode(sidBytes);
      const sess = state.sessions.get(sid);
      if (!sess) return;
      let off = 3 + sidLen;
      const subkind = view.getUint8(off); off += 1;
      if (subkind === 0) {
        applySessionCursor(sess, 'default');
      } else if (subkind === 1) {
        applySessionCursor(sess, 'none');
      } else if (subkind === 2) {
        if (off + 8 > buf.byteLength) return;
        const pw = view.getUint16(off, true); off += 2;
        const ph = view.getUint16(off, true); off += 2;
        const hotX = view.getUint16(off, true); off += 2;
        const hotY = view.getUint16(off, true); off += 2;
        const expected = pw * ph * 4;
        if (pw === 0 || ph === 0 || off + expected > buf.byteLength) {
          applySessionCursor(sess, 'none');
          return;
        }
        const rgba = new Uint8Array(buf, off, expected);
        const css = buildPointerCssCursor(pw, ph, hotX, hotY, rgba);
        applySessionCursor(sess, css);
      }
    }
  };

  ws.onclose = () => {
    setTimeout(connectStreamWs, 1000);
  };
}

function schedulePresent(sess) {
  if (!sess.dirty || sess.presentRaf) return;
  sess.presentRaf = requestAnimationFrame(() => {
    sess.presentRaf = 0;
    if (!sess.dirty) return;
    const x = sess.dirtyMinX;
    const y = sess.dirtyMinY;
    const w = sess.dirtyMaxX - x;
    const h = sess.dirtyMaxY - y;
    sess.dirty = false;
    if (w > 0 && h > 0) {
      sess.ctx.drawImage(sess.backCanvas, x, y, w, h, x, y, w, h);
    }
  });
}

function handleSessionStatus(payload) {
  const sess = state.sessions.get(payload.session_id);
  if (!sess) return;
  sess.state = payload.state;
  sess.code = payload.code || null;
  sess.message = payload.message || '';
  if (payload.width) sess.fbW = payload.width;
  if (payload.height) sess.fbH = payload.height;
  if (payload.rtt_ms != null) sess.rttMs = payload.rtt_ms;

  if (payload.state === 'error' && payload.code === 'AUTH_FAILED') {
    notify(payload.message || '帳號或密碼錯誤', true);
  }

  renderTabs();
  if (state.activeTabId === payload.session_id) {
    renderActiveRdpChrome(sess);
    if (payload.state === 'connected' && !document.querySelector('.modal.open')) {
      sess.locksSynced = false;
      setTimeout(() => sess.canvas?.focus(), 20);
    }
    // The window may have been resized while this session was still connecting (the observer
    // skips sessions that are not connected yet), so re-check once the session is up.
    if (payload.state === 'connected') scheduleAutoResize(250);
  }
}

// ── Custom Window Controls & Fullscreen ───────────────────────────────────────

function wireWindowControls() {
  const startDrag = (e) => {
    if (e.button !== 0) return;
    if (e.detail === 2) {
      toggleMaximize();
    } else {
      invoke?.('win_start_drag');
    }
  };
  $('dragBrand').addEventListener('mousedown', startDrag);
  $('dragSpacer').addEventListener('mousedown', startDrag);

  $('btnWinMin').onclick = () => invoke?.('win_minimize');
  $('btnWinMax').onclick = toggleMaximize;
  $('btnWinClose').onclick = () => invoke?.('win_close');
}

async function toggleMaximize() {
  if (!invoke) return;
  state.isMaximized = await invoke('win_toggle_maximize');
}

function applyFullscreenUi(fs) {
  state.isFullscreen = Boolean(fs);
  document.body.classList.toggle('is-fullscreen', state.isFullscreen);
  $('btnHudFullscreen').classList.toggle('on', state.isFullscreen);
  $('btnHudFullscreen').title = state.isFullscreen ? '離開全螢幕' : '全螢幕';
}

async function toggleFullscreen() {
  if (!invoke) return;
  try {
    const fs = await invoke('win_toggle_fullscreen');
    applyFullscreenUi(fs);
  } catch (e) {
    notify(String(e), true);
  }
}

async function ensureExitFullscreen() {
  if (!state.isFullscreen || !invoke) return;
  try {
    await invoke('win_set_fullscreen', { fullscreen: false });
  } catch (_) {}
  applyFullscreenUi(false);
}

let fsSyncTimer = null;
function wireFullscreenSync() {
  window.__TAURI__?.event?.listen?.('fullscreen-changed', (ev) => {
    if (typeof ev.payload === 'boolean' && ev.payload !== state.isFullscreen) {
      applyFullscreenUi(ev.payload);
    }
  });
  window.addEventListener('resize', () => {
    if (!invoke) return;
    clearTimeout(fsSyncTimer);
    fsSyncTimer = setTimeout(async () => {
      try {
        const fs = await invoke('win_is_fullscreen');
        if (typeof fs === 'boolean' && fs !== state.isFullscreen) {
          applyFullscreenUi(fs);
        }
      } catch (_) {}
    }, 140);
  });
}

// ── Vault Gate & Data Rendering ───────────────────────────────────────────────

function renderVaultGate() {
  const unlocked = state.vaultStatus.unlocked;
  $('viewVaultGate').classList.toggle('hidden', unlocked);
  $('viewHome').classList.toggle('hidden', !unlocked || state.activeTabId !== 'home');
  $('viewRdp').classList.toggle('hidden', !unlocked || state.activeTabId === 'home');

  if (!unlocked) {
    const exists = state.vaultStatus.exists;
    $('gateTitle').textContent = exists ? '解鎖保管庫' : '建立保管庫';
    $('gateConfirmWrap').classList.toggle('hidden', exists);
    $('btnGateSubmit').textContent = exists ? '解鎖' : '建立';
    $('gateError').classList.add('hidden');
    $('gatePwd').value = '';
    $('gatePwdConfirm').value = '';
    state.gateRemember = Boolean(state.vaultStatus.remember_keychain);
    $('toggleGateRemember').classList.toggle('on', state.gateRemember);
    setTimeout(() => $('gatePwd').focus(), 40);
  }
}

function applyVaultData(data) {
  state.data = data;
  state.vaultStatus.unlocked = true;
  state.vaultStatus.exists = true;
  state.vaultStatus.vault_path = data.vault_path;
  state.vaultStatus.remember_keychain = Boolean(data.remember_keychain);
  $('settingsVaultPath').textContent = data.vault_path;
  $('toggleRememberKeychain').classList.toggle('on', Boolean(data.remember_keychain));
  $('toggleMacCmd').classList.toggle('on', data.settings.mac_cmd_to_ctrl);
  $('toggleSelfSigned').classList.toggle('on', data.settings.accept_self_signed_tls);

  renderVaultGate();
  renderSidebarGroups();
  renderHosts();
}

function renderSidebarGroups() {
  $('navHostCount').textContent = state.data.hosts.length;

  const groups = new Map();
  for (const h of state.data.hosts) {
    const g = h.group || '預設';
    groups.set(g, (groups.get(g) || 0) + 1);
  }
  const wrap = $('groupNavWrap');
  const el = $('groupNavList');
  if (groups.size === 0) {
    wrap.classList.add('hidden');
    el.innerHTML = '';
    return;
  }
  wrap.classList.remove('hidden');
  el.innerHTML = [...groups.entries()]
    .map(([g, count]) => `
      <div class="nav-item ${state.activeNav === 'hosts' && state.activeGroup === g ? 'active' : ''}" data-group="${escapeHtml(g)}">
        <span class="nav-left"><span class="tab-dot"></span><span>${escapeHtml(g)}</span></span>
        <span class="nav-count">${count}</span>
      </div>
    `).join('');

  el.querySelectorAll('[data-group]').forEach((node) => {
    node.onclick = () => {
      state.activeNav = 'hosts';
      state.activeGroup = node.getAttribute('data-group');
      updateNavPanels();
      renderSidebarGroups();
      renderHosts();
    };
  });
}

function renderHosts() {
  const list = state.activeGroup
    ? state.data.hosts.filter((h) => (h.group || '預設') === state.activeGroup)
    : state.data.hosts;

  $('hostsTitle').textContent = state.activeGroup || '全部主機';
  $('hostEmpty').classList.toggle('hidden', list.length > 0);

  $('hostGrid').innerHTML = list.map((h) => {
    const addrDisplay = h.port && h.port !== 3389 ? `${h.address}:${h.port}` : h.address;
    const userDisplay = h.username
      ? (h.domain ? `${h.domain}\\${h.username}` : h.username)
      : '—';
    const resTag = resolutionLabel(h.width, h.height);
    const shareTag = h.shared_folder ? `<span class="tag accent">共享</span>` : '';

    return `
      <div class="host-card" data-host-card="${h.id}">
        <div class="host-top">
          <div>
            <div class="host-name"><span class="tab-dot"></span>${escapeHtml(h.name)}</div>
            <div class="host-addr">${escapeHtml(addrDisplay)}</div>
          </div>
          <div class="host-tags">
            ${shareTag}
            <span class="tag">${escapeHtml(resTag)}</span>
          </div>
        </div>
        <div class="host-meta">
          <span class="host-user">${escapeHtml(userDisplay)}</span>
          <div class="host-actions">
            <button class="btn btn-sm" data-edit-host="${h.id}">編輯</button>
            <button class="btn btn-sm btn-danger" data-del-host="${h.id}">刪除</button>
            <button class="btn btn-sm btn-primary" data-conn-host="${h.id}">連線</button>
          </div>
        </div>
      </div>
    `;
  }).join('');

  // Double-click host card to connect (ignoring clicks on buttons)
  $('hostGrid').querySelectorAll('[data-host-card]').forEach((card) => {
    card.ondblclick = (e) => {
      if (e.target.closest('button')) return;
      const h = state.data.hosts.find((x) => x.id === card.dataset.hostCard);
      if (h) initiateHostConnect(h);
    };
  });

  $('hostGrid').querySelectorAll('[data-conn-host]').forEach((b) => {
    b.onclick = () => {
      const h = state.data.hosts.find((x) => x.id === b.dataset.connHost);
      if (h) initiateHostConnect(h);
    };
  });
  $('hostGrid').querySelectorAll('[data-edit-host]').forEach((b) => {
    b.onclick = () => {
      const h = state.data.hosts.find((x) => x.id === b.dataset.editHost);
      if (h) openHostModal(h);
    };
  });
  $('hostGrid').querySelectorAll('[data-del-host]').forEach((b) => {
    b.onclick = async () => {
      try {
        const data = await invoke('vault_delete_host', { id: b.dataset.delHost });
        applyVaultData(data);
        scheduleAutoSync();
        notify('已刪除');
      } catch (e) { notify(String(e), true); }
    };
  });
}

// ── Tabs & Multi-Session RDP ──────────────────────────────────────────────────

function renderTabs() {
  $('tabStrip').innerHTML = state.tabs.map((t) => {
    let dot = '';
    if (t.type === 'rdp') {
      const s = state.sessions.get(t.id);
      const cls = s?.state === 'connected' ? '' : s?.state === 'error' ? 'error' : 'connecting';
      dot = `<span class="tab-dot ${cls}"></span>`;
    } else {
      dot = `<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 9l9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/></svg>`;
    }
    return `
      <div class="tab ${t.id === state.activeTabId ? 'active' : ''}" data-tab="${t.id}">
        ${dot}
        <span class="tab-label">${escapeHtml(t.title)}</span>
        ${t.type === 'rdp' ? `<span class="tab-close" data-close-tab="${t.id}">×</span>` : ''}
      </div>
    `;
  }).join('');

  $('tabStrip').querySelectorAll('[data-tab]').forEach((el) => {
    el.onclick = () => selectTab(el.dataset.tab);
  });
  $('tabStrip').querySelectorAll('[data-close-tab]').forEach((el) => {
    el.onclick = (e) => {
      e.stopPropagation();
      closeRdpTab(el.dataset.closeTab);
    };
  });
}

function selectTab(id) {
  const prevId = state.activeTabId;
  if (prevId !== 'home' && prevId !== id) {
    releaseSessionModifiers(prevId);
  }

  state.activeTabId = id;
  renderTabs();

  const isHome = id === 'home';
  if (isHome) {
    ensureExitFullscreen();
  }
  $('viewHome').classList.toggle('hidden', !isHome || !state.vaultStatus.unlocked);
  $('viewRdp').classList.toggle('hidden', isHome || !state.vaultStatus.unlocked);

  for (const [sid, sess] of state.sessions.entries()) {
    sess.canvas.classList.toggle('active', sid === id);
  }

  if (isHome) {
    invoke?.('set_canvas_focused', { focused: false });
    invoke?.('set_active_rdp_tab', { sessionId: null });
    syncHookToggleUi(null);
  } else {
    const sess = state.sessions.get(id);
    if (sess) {
      sess.locksSynced = false;
      invoke?.('set_active_rdp_tab', { sessionId: id });
      invoke?.('set_keyboard_hook', { enabled: sess.hookEnabled });
      renderActiveRdpChrome(sess);
      scheduleAutoResize(250);
      setTimeout(() => {
        sess.canvas.focus();
        if (document.activeElement === sess.canvas) {
          invoke?.('set_canvas_focused', { focused: true });
        }
      }, 20);
    }
  }
}

function renderActiveRdpChrome(sess) {
  syncHookToggleUi(sess);
  syncLosslessToggleUi(sess);
  syncAudioToggleUi(sess);
  $('hudShareBadge').classList.toggle('hidden', !sess.sharedFolder);
  if (sess.sharedFolder) {
    $('hudShareBadge').title = sess.sharedFolder;
  }

  const hudSelect = $('hudResSelect');
  const presetVal = presetForSize(sess.requestedW, sess.requestedH);
  if ([...hudSelect.options].some((o) => o.value === presetVal)) {
    hudSelect.value = presetVal;
  } else {
    hudSelect.value = 'auto';
  }

  const rtt = sess.rttMs != null ? ` · ${sess.rttMs}ms` : '';
  $('hudTelemetry').textContent =
    sess.state === 'connected' ? `${sess.fbW}×${sess.fbH}${rtt}` : (sess.state === 'connecting' ? '連線中' : '已斷線');

  const overlay = $('sessionOverlay');
  const card = $('overlayCard');
  if (sess.state === 'connected') {
    overlay.classList.add('hidden');
    scheduleHudCollapse(1200);
  } else {
    overlay.classList.remove('hidden');
    showHud();
    const isConnecting = sess.state === 'connecting';
    const isAuthError = sess.state === 'error' && sess.code === 'AUTH_FAILED';
    card.classList.toggle('auth-error', isAuthError);
    $('overlaySpinner').classList.toggle('hidden', !isConnecting);
    $('overlayActions').classList.toggle('hidden', isConnecting);
    $('btnOverlayReprompt').classList.toggle('hidden', !isAuthError);
    $('btnOverlayRetry').classList.toggle('hidden', isAuthError);
    $('overlayTitle').textContent = isConnecting
      ? '連線中...'
      : (isAuthError ? (sess.message || '帳號或密碼錯誤') : '連線中斷');
    $('overlayMsg').textContent = isAuthError ? `${sess.params.host}${sess.params.port !== 3389 ? ':' + sess.params.port : ''}` : (sess.message || '');
  }
}

function initiateHostConnect(host) {
  const [w, h] = resolveRequestedSize(host.width || 0, host.height || 0);
  const needUser = !host.username;
  const needPwd = !host.has_password;

  if (needUser || needPwd) {
    openPromptModal({
      title: host.name || host.address,
      errorMsg: '',
      needUser,
      defaultUser: host.domain && host.username ? `${host.domain}\\${host.username}` : (host.username || ''),
      canUpdateSaved: false,
      onSubmit: ({ username, password }) => {
        launchRdpSession({
          title: host.name || host.address,
          host_id: host.id && host.id !== 'quick' ? host.id : null,
          host: host.address,
          port: host.port || 3389,
          username: username || host.username || null,
          password,
          width: w,
          height: h,
          requestedW: host.width || 0,
          requestedH: host.height || 0,
          clipboard_enabled: host.clipboard_enabled ?? true,
          hook_enabled: host.keyboard_hook_enabled ?? true,
          audio_enabled: host.audio_enabled ?? true,
          shared_folder: host.shared_folder || ''
        });
      }
    });
    return;
  }

  launchRdpSession({
    title: host.name || host.address,
    host_id: host.id,
    host: host.address,
    port: host.port || 3389,
    username: host.username,
    password: null,
    width: w,
    height: h,
    requestedW: host.width || 0,
    requestedH: host.height || 0,
    clipboard_enabled: host.clipboard_enabled ?? true,
    hook_enabled: host.keyboard_hook_enabled ?? true,
    audio_enabled: host.audio_enabled ?? true,
    shared_folder: host.shared_folder || ''
  });
}

async function launchRdpSession(opts) {
  const sid = `rdp-${Date.now()}-${Math.random().toString(16).slice(2, 6)}`;
  const canvas = document.createElement('canvas');
  canvas.className = 'rdp-canvas';
  canvas.tabIndex = 0;
  canvas.width = opts.width;
  canvas.height = opts.height;
  bindCanvasInput(canvas, sid);
  $('canvasDeck').appendChild(canvas);

  // Offscreen backbuffer paired with rAF atomic swap eliminates scanlines/tearing
  const backCanvas = document.createElement('canvas');
  backCanvas.width = opts.width;
  backCanvas.height = opts.height;

  const sess = {
    id: sid,
    canvas,
    ctx: canvas.getContext('2d', { alpha: false, desynchronized: false }),
    backCanvas,
    backCtx: backCanvas.getContext('2d', { alpha: false }),
    dirty: false,
    dirtyMinX: 0,
    dirtyMinY: 0,
    dirtyMaxX: 0,
    dirtyMaxY: 0,
    lastCursor: 'default',
    presentRaf: 0,
    fbW: opts.width,
    fbH: opts.height,
    requestedW: opts.requestedW || 0,
    requestedH: opts.requestedH || 0,
    lastAutoResize: null,
    state: 'connecting',
    code: null,
    message: `${opts.host}${opts.port !== 3389 ? ':' + opts.port : ''}`,
    rttMs: null,
    hookEnabled: opts.hook_enabled ?? true,
    clipEnabled: opts.clipboard_enabled ?? true,
    audioEnabled: opts.audio_enabled ?? true,
    losslessQuality: opts.lossless_quality ?? (state.data.settings.lossless_quality ?? true),
    sharedFolder: opts.shared_folder || '',
    params: opts
  };
  state.sessions.set(sid, sess);
  state.tabs.push({ id: sid, type: 'rdp', title: opts.title });
  selectTab(sid);

  await connectSessionTransport(sess, opts.width, opts.height);
}

async function connectSessionTransport(sess, width, height) {
  const opts = sess.params;
  try {
    await invoke('rdp_connect', {
      req: {
        session_id: sess.id,
        host_id: opts.host_id || null,
        host: opts.host,
        port: opts.port || 3389,
        username: opts.username || null,
        domain: null,
        password: opts.password || null,
        width,
        height,
        clipboard_enabled: sess.clipEnabled,
        audio_enabled: sess.audioEnabled,
        lossless_quality: sess.losslessQuality,
        shared_folder: sess.sharedFolder || null
      }
    });
  } catch (err) {
    sess.state = 'error';
    const msg = String(err);
    sess.code = (msg === 'NEED_PASSWORD' || msg === 'NEED_CREDENTIALS') ? 'AUTH_FAILED' : 'ERROR';
    sess.message = msg === 'NEED_PASSWORD' ? '請輸入密碼' : msg;
    renderTabs();
    if (state.activeTabId === sess.id) {
      renderActiveRdpChrome(sess);
    }
  }
}

async function reconnectSessionInPlace(sess) {
  const [w, h] = sess.fbW >= 640 && sess.fbH >= 480
    ? [sess.fbW, sess.fbH]
    : resolveRequestedSize(sess.requestedW || 0, sess.requestedH || 0);
  sess.state = 'connecting';
  sess.code = null;
  sess.message = `${sess.params.host}${sess.params.port !== 3389 ? ':' + sess.params.port : ''}`;
  sess.locksSynced = false;
  applySessionCursor(sess, 'default');
  renderTabs();
  if (state.activeTabId === sess.id) {
    renderActiveRdpChrome(sess);
  }
  await connectSessionTransport(sess, w, h);
}

function closeRdpTab(sid) {
  invoke?.('rdp_disconnect', { sessionId: sid });
  const sess = state.sessions.get(sid);
  if (sess) {
    if (sess.presentRaf) cancelAnimationFrame(sess.presentRaf);
    sess.canvas.remove();
    state.sessions.delete(sid);
  }
  const idx = state.tabs.findIndex((t) => t.id === sid);
  if (idx > 0) state.tabs.splice(idx, 1);
  if (state.activeTabId === sid) {
    selectTab(state.tabs[Math.max(0, idx - 1)].id);
  } else {
    renderTabs();
  }
}

// ── Keyboard & Mouse Input ────────────────────────────────────────────────────

function bindCanvasInput(canvas, sid) {
  const mapCoords = (e) => {
    const rect = canvas.getBoundingClientRect();
    const sess = state.sessions.get(sid);
    if (!sess || rect.width <= 0 || rect.height <= 0) return [0, 0];
    const scale = Math.min(rect.width / sess.fbW, rect.height / sess.fbH);
    const drawW = sess.fbW * scale;
    const drawH = sess.fbH * scale;
    const offX = (rect.width - drawW) / 2;
    const offY = (rect.height - drawH) / 2;
    const x = Math.round(((e.clientX - rect.left - offX) / drawW) * sess.fbW);
    const y = Math.round(((e.clientY - rect.top - offY) / drawH) * sess.fbH);
    return [
      Math.max(0, Math.min(sess.fbW - 1, x)),
      Math.max(0, Math.min(sess.fbH - 1, y))
    ];
  };

  canvas.addEventListener('contextmenu', (e) => e.preventDefault());
  canvas.addEventListener('focus', () => {
    invoke?.('set_canvas_focused', { focused: true });
  });
  canvas.addEventListener('blur', () => {
    invoke?.('set_canvas_focused', { focused: false });
  });

  let lastMove = 0;
  canvas.addEventListener('mousemove', (e) => {
    const now = performance.now();
    if (now - lastMove < 8) return;
    lastMove = now;
    const [x, y] = mapCoords(e);
    invoke?.('rdp_mouse', { sessionId: sid, x, y, button: 0, action: 'move', wheelDelta: 0 });
  });

  canvas.addEventListener('mousedown', (e) => {
    e.preventDefault();
    canvas.focus();
    const [x, y] = mapCoords(e);
    invoke?.('rdp_mouse', { sessionId: sid, x, y, button: e.button, action: 'down', wheelDelta: 0 });
  });

  canvas.addEventListener('mouseup', (e) => {
    e.preventDefault();
    const [x, y] = mapCoords(e);
    invoke?.('rdp_mouse', { sessionId: sid, x, y, button: e.button, action: 'up', wheelDelta: 0 });
  });

  canvas.addEventListener('wheel', (e) => {
    e.preventDefault();
    const [x, y] = mapCoords(e);
    if (Math.abs(e.deltaY) >= Math.abs(e.deltaX)) {
      const delta = e.deltaY > 0 ? -120 : 120;
      invoke?.('rdp_mouse', { sessionId: sid, x, y, button: 0, action: 'wheel', wheelDelta: delta });
    } else if (e.deltaX !== 0) {
      const delta = e.deltaX > 0 ? 120 : -120;
      invoke?.('rdp_mouse', { sessionId: sid, x, y, button: 0, action: 'hwheel', wheelDelta: delta });
    }
  }, { passive: false });

  const resolveScancode = (e) => {
    const code = e.code || '';
    if (state.platform === 'macos' && state.data.settings.mac_cmd_to_ctrl) {
      if (code === 'MetaLeft') return [0x1d, false];
      if (code === 'MetaRight') return [0x1d, true];
    }
    if (code && SCANCODE_MAP[code]) {
      return SCANCODE_MAP[code];
    }
    // Fallback for Chromium/WebView2 on Windows 11 with Microsoft IME (Chromium #380704156),
    // where Right Shift reports e.code === "" and e.location === 0 while Left Shift reports ShiftLeft/1.
    if (e.key === 'Shift' || e.keyCode === 16) {
      return e.location === 1 ? [0x2a, false] : [0x36, false];
    }
    if (e.key === 'Control' || e.keyCode === 17) {
      return e.location === 2 ? [0x1d, true] : [0x1d, false];
    }
    if (e.key === 'Alt' || e.keyCode === 18) {
      return e.location === 2 ? [0x38, true] : [0x38, false];
    }
    return null;
  };

  const isPureModifierKey = (e) =>
    e.key === 'Shift' || e.key === 'Control' || e.key === 'Alt' || e.key === 'Meta'
    || e.keyCode === 16 || e.keyCode === 17 || e.keyCode === 18;

  canvas.addEventListener('keydown', (e) => {
    if (e.isComposing && !isPureModifierKey(e)) return;

    // On macOS (or non-Windows), sync CapsLock/NumLock/ScrollLock state on first keypress after focus
    const sess = state.sessions.get(sid);
    if (state.platform !== 'windows' && sess && !sess.locksSynced && typeof e.getModifierState === 'function') {
      sess.locksSynced = true;
      invoke?.('rdp_sync_locks', {
        sessionId: sid,
        scroll: Boolean(e.getModifierState('ScrollLock')),
        num: Boolean(e.getModifierState('NumLock')),
        caps: Boolean(e.getModifierState('CapsLock'))
      });
    }

    // Native mstsc meta-shortcut fallbacks (active on macOS or when OS hook isn't active)
    if (e.ctrlKey && e.altKey && !e.metaKey) {
      if (e.code === 'End') {
        e.preventDefault();
        invoke?.('rdp_ctrl_alt_del', { sessionId: sid });
        return;
      }
      if (e.code === 'Home') {
        e.preventDefault();
        showHud();
        scheduleHudCollapse(2600);
        return;
      }
      if (e.code === 'Pause') {
        e.preventDefault();
        toggleFullscreen();
        return;
      }
      if (e.code === 'NumpadSubtract') {
        e.preventDefault();
        invoke?.('rdp_remote_action', { sessionId: sid, action: 'screenshot_window' });
        return;
      }
      if (e.code === 'NumpadAdd') {
        e.preventDefault();
        invoke?.('rdp_remote_action', { sessionId: sid, action: 'screenshot_screen' });
        return;
      }
    }
    if (e.altKey && !e.ctrlKey && !e.metaKey) {
      if (e.code === 'Home') {
        e.preventDefault();
        invoke?.('rdp_remote_action', { sessionId: sid, action: 'start_menu' });
        return;
      }
      if (e.code === 'Delete') {
        e.preventDefault();
        invoke?.('rdp_remote_action', { sessionId: sid, action: 'window_menu' });
        return;
      }
      if (e.code === 'Insert') {
        e.preventDefault();
        invoke?.('rdp_remote_action', { sessionId: sid, action: 'cycle_order' });
        return;
      }
      if (e.code === 'PageUp') {
        e.preventDefault();
        invoke?.('rdp_remote_action', { sessionId: sid, action: 'switch_app_next' });
        return;
      }
      if (e.code === 'PageDown') {
        e.preventDefault();
        invoke?.('rdp_remote_action', { sessionId: sid, action: 'switch_app_prev' });
        return;
      }
    }
    if (e.code === 'Pause') {
      e.preventDefault();
      invoke?.('rdp_pause', { sessionId: sid });
      return;
    }

    const sc = resolveScancode(e);
    if (!sc) return;
    e.preventDefault();
    invoke?.('rdp_key', { sessionId: sid, scancode: sc[0], extended: sc[1], pressed: true });
  });

  canvas.addEventListener('keyup', (e) => {
    if (e.isComposing && !isPureModifierKey(e)) return;
    if (e.code === 'Pause') {
      e.preventDefault();
      return;
    }
    const sc = resolveScancode(e);
    if (!sc) return;
    e.preventDefault();
    invoke?.('rdp_key', { sessionId: sid, scancode: sc[0], extended: sc[1], pressed: false });
  });

  canvas.addEventListener('compositionend', (e) => {
    const text = e.data || '';
    for (let i = 0; i < text.length; i++) {
      const cu = text.charCodeAt(i);
      invoke?.('rdp_unicode', { sessionId: sid, codeUnit: cu, pressed: true });
      invoke?.('rdp_unicode', { sessionId: sid, codeUnit: cu, pressed: false });
    }
  });
}

// ── UI Event Wiring ───────────────────────────────────────────────────────────

function wireUiEvents() {
  $('toggleGateRemember').onclick = () => {
    state.gateRemember = !state.gateRemember;
    $('toggleGateRemember').classList.toggle('on', state.gateRemember);
  };

  $('gateForm').onsubmit = async (e) => {
    e.preventDefault();
    const pwd = $('gatePwd').value;
    const errEl = $('gateError');
    errEl.classList.add('hidden');

    if (!state.vaultStatus.exists) {
      if (pwd.length < 4) {
        errEl.textContent = '主密碼至少 4 碼';
        errEl.classList.remove('hidden');
        return;
      }
      if (pwd !== $('gatePwdConfirm').value) {
        errEl.textContent = '兩次密碼不一致';
        errEl.classList.remove('hidden');
        return;
      }
      try {
        const data = await invoke('vault_create', {
          masterPassword: pwd,
          rememberKeychain: state.gateRemember
        });
        applyVaultData(data);
        maybeAutoSyncOnUnlock();
      } catch (err) {
        errEl.textContent = String(err);
        errEl.classList.remove('hidden');
      }
    } else {
      try {
        const data = await invoke('vault_unlock', {
          masterPassword: pwd,
          rememberKeychain: state.gateRemember
        });
        applyVaultData(data);
        maybeAutoSyncOnUnlock();
      } catch (err) {
        errEl.textContent = String(err);
        errEl.classList.remove('hidden');
      }
    }
  };

  $('btnGateImport').onclick = () => { $('importModal').classList.add('open'); setModalOpenFlag(true); };

  document.querySelectorAll('[data-nav]').forEach((el) => {
    el.onclick = () => {
      state.activeNav = el.dataset.nav;
      state.activeGroup = null;
      updateNavPanels();
      renderSidebarGroups();
      renderHosts();
    };
  });

  $('btnLockVault').onclick = async () => {
    clearTimeout(autoSyncTimer);
    state.vaultStatus = await invoke('vault_lock');
    selectTab('home');
    renderVaultGate();
  };

  // Quick Connect
  $('btnQuickConnect').onclick = doQuickConnect;
  $('quickAddr').addEventListener('keydown', (e) => { if (e.key === 'Enter') doQuickConnect(); });
  $('quickPort').addEventListener('keydown', (e) => { if (e.key === 'Enter') doQuickConnect(); });
  $('btnAddTab').onclick = () => { selectTab('home'); $('quickAddr').focus(); };

  // Host Modal
  $('btnNewHost').onclick = () => openHostModal(null);
  $('btnHostCancel').onclick = () => { $('hostModal').classList.remove('open'); setModalOpenFlag(false); };
  $('hostResPreset').onchange = () => {
    $('hostCustomResRow').classList.toggle('hidden', $('hostResPreset').value !== 'custom');
  };
  $('hostSharePreset').onchange = () => {
    const val = $('hostSharePreset').value;
    $('hostSharePathWrap').classList.toggle('hidden', val !== '__custom__');
    if (val === '__custom__') {
      $('hostSharePath').focus();
    }
  };
  $('toggleHostSavePwd').onclick = () => {
    state.hostSavePwd = !state.hostSavePwd;
    syncHostPwdToggle();
  };
  $('toggleHostClip').onclick = () => {
    state.hostClip = !state.hostClip;
    $('toggleHostClip').classList.toggle('on', state.hostClip);
  };
  $('toggleHostHook').onclick = () => {
    state.hostHook = !state.hostHook;
    $('toggleHostHook').classList.toggle('on', state.hostHook);
  };
  $('toggleHostAudio').onclick = () => {
    state.hostAudio = !state.hostAudio;
    $('toggleHostAudio').classList.toggle('on', state.hostAudio);
  };
  $('btnHostSave').onclick = saveHostModal;

  // Prompt Modal
  $('btnPromptCancel').onclick = () => {
    $('promptModal').classList.remove('open');
    setModalOpenFlag(false);
    state.pendingPrompt = null;
  };
  $('togglePromptUpdate').onclick = () => {
    state.promptUpdatePwd = !state.promptUpdatePwd;
    $('togglePromptUpdate').classList.toggle('on', state.promptUpdatePwd);
  };
  $('btnPromptConfirm').onclick = submitPromptModal;
  $('promptPwd').addEventListener('keydown', (e) => { if (e.key === 'Enter') submitPromptModal(); });

  // Export / Import Vault
  $('btnExportVault').onclick = exportVault;
  $('btnImportVault').onclick = () => { $('importModal').classList.add('open'); setModalOpenFlag(true); };
  $('btnImportCancel').onclick = () => { $('importModal').classList.remove('open'); setModalOpenFlag(false); };
  $('btnImportConfirm').onclick = confirmImportVault;

  // Settings Toggles
  $('toggleRememberKeychain').onclick = async () => {
    const next = !state.data.remember_keychain;
    try {
      const data = await invoke('vault_set_remember_keychain', { enabled: next });
      applyVaultData(data);
      notify(next ? '已儲存至系統鑰匙圈' : '已從系統鑰匙圈移除');
    } catch (e) {
      notify(String(e), true);
    }
  };
  $('toggleMacCmd').onclick = async () => {
    const next = { ...state.data.settings, mac_cmd_to_ctrl: !state.data.settings.mac_cmd_to_ctrl };
    const data = await invoke('vault_update_settings', { settings: next });
    applyVaultData(data);
    scheduleAutoSync();
  };
  $('toggleSelfSigned').onclick = async () => {
    const next = { ...state.data.settings, accept_self_signed_tls: !state.data.settings.accept_self_signed_tls };
    const data = await invoke('vault_update_settings', { settings: next });
    applyVaultData(data);
    scheduleAutoSync();
  };

  // Zero-Knowledge Sync Controls
  $('syncMode').onchange = () => syncFormVisibility($('syncMode').value);
  $('syncFolderPreset').onchange = () => {
    const val = $('syncFolderPreset').value;
    if (val && val !== '__custom__') {
      $('syncFolderPath').value = val;
    }
  };
  $('toggleSyncDavInsecure').onclick = () => {
    state.syncDavInsecure = !state.syncDavInsecure;
    $('toggleSyncDavInsecure').classList.toggle('on', state.syncDavInsecure);
  };
  $('toggleSyncAuto').onclick = () => {
    state.syncAuto = !state.syncAuto;
    $('toggleSyncAuto').classList.toggle('on', state.syncAuto);
  };
  $('btnSyncClearPwd').onclick = async () => {
    try {
      const inp = collectSyncFormInput();
      inp.webdav_password = null;
      inp.clear_webdav_password = true;
      const cfg = await invoke('sync_save_config', { input: inp });
      state.syncConfig = cfg;
      renderSyncConfig();
      notify('已清除已存 WebDAV 密碼');
    } catch (e) { notify(String(e), true); }
  };
  $('btnSyncTest').onclick = testSyncConnectionUi;
  $('btnSyncSave').onclick = saveSyncConfigUi;
  $('btnSyncNow').onclick = async () => {
    if ($('syncMode').value === 'disabled') {
      notify('請先選擇同步方式', true);
      return;
    }
    const ok = await saveSyncConfigUi({ quiet: true });
    if (ok) await runSyncNow({ silent: false });
  };
  $('btnSidebarSync').onclick = () => runSyncNow({ silent: false });

  // RDP HUD controls
  $('btnHudPin').onclick = () => {
    state.hudPinned = !state.hudPinned;
    localStorage.setItem('mistrdp.hudPinned', state.hudPinned ? '1' : '0');
    syncHudPinUi();
  };
  $('btnTitlebarHook').onclick = toggleActiveHookMode;
  $('btnHudLossless').onclick = toggleActiveLosslessMode;
  $('btnHudAudio').onclick = toggleActiveAudio;
  $('btnHudCad').onclick = () => {
    if (state.activeTabId !== 'home') {
      invoke?.('rdp_ctrl_alt_del', { sessionId: state.activeTabId });
    }
  };
  $('hudResSelect').onchange = () => {
    const sess = state.sessions.get(state.activeTabId);
    if (!sess) return;
    const val = $('hudResSelect').value;
    const preset = RESOLUTION_PRESETS.find((p) => p.value === val);
    const [w, h] = preset && preset.w > 0 ? [preset.w, preset.h] : calcAutoResolution();
    sess.requestedW = preset ? preset.w : 0;
    sess.requestedH = preset ? preset.h : 0;
    sess.lastAutoResize = null;
    invoke?.('rdp_resize', { sessionId: sess.id, width: w, height: h });
    notify(`解析度 ${w}×${h}`);
    setTimeout(() => sess.canvas?.focus(), 20);
  };
  $('btnHudMinimize').onclick = () => {
    releaseActiveSessionModifiers();
    invoke?.('win_minimize');
  };
  $('btnHudFullscreen').onclick = toggleFullscreen;
  $('btnHudDisconnect').onclick = () => {
    if (state.activeTabId !== 'home') closeRdpTab(state.activeTabId);
  };
  $('btnOverlayClose').onclick = () => {
    if (state.activeTabId !== 'home') closeRdpTab(state.activeTabId);
  };
  $('btnOverlayRetry').onclick = () => {
    const sess = state.sessions.get(state.activeTabId);
    if (!sess) return;
    const params = sess.params;
    closeRdpTab(sess.id);
    launchRdpSession(params);
  };
  $('btnOverlayReprompt').onclick = () => {
    const sess = state.sessions.get(state.activeTabId);
    if (!sess) return;
    const params = { ...sess.params };
    const savedHost = params.host_id
      ? state.data.hosts.find((h) => h.id === params.host_id)
      : null;

    openPromptModal({
      title: params.title,
      errorMsg: sess.message || '帳號或密碼錯誤',
      needUser: true,
      defaultUser: params.username || savedHost?.username || '',
      canUpdateSaved: Boolean(savedHost && savedHost.save_password),
      onSubmit: async ({ username, password, updateSaved }) => {
        if (updateSaved && savedHost) {
          try {
            const data = await invoke('vault_update_host_credentials', {
              hostId: savedHost.id,
              username: username || null,
              password
            });
            applyVaultData(data);
            scheduleAutoSync();
          } catch (_) {}
        }
        closeRdpTab(sess.id);
        launchRdpSession({
          ...params,
          username: username || params.username,
          password
        });
      }
    });
  };
}

function updateNavPanels() {
  document.querySelectorAll('[data-nav]').forEach((n) => {
    n.classList.toggle('active', n.dataset.nav === state.activeNav && !state.activeGroup);
  });
  $('panelHosts').classList.toggle('hidden', state.activeNav !== 'hosts');
  $('panelSettings').classList.toggle('hidden', state.activeNav !== 'settings');
}

function doQuickConnect() {
  const address = $('quickAddr').value.trim();
  if (!address) { notify('請輸入主機位址', true); return; }
  const portVal = parseInt($('quickPort').value.trim(), 10);
  const port = portVal > 0 && portVal <= 65535 ? portVal : 3389;
  const preset = RESOLUTION_PRESETS.find((p) => p.value === $('quickRes').value);

  initiateHostConnect({
    id: 'quick',
    name: address,
    address,
    port,
    group: '',
    username: '',
    domain: '',
    has_password: false,
    width: preset ? preset.w : 0,
    height: preset ? preset.h : 0,
    clipboard_enabled: true,
    keyboard_hook_enabled: true,
    audio_enabled: true,
    shared_folder: ''
  });
}

function syncHostPwdToggle(hadPwd = false) {
  $('toggleHostSavePwd').classList.toggle('on', state.hostSavePwd);
  $('hostPwd').disabled = !state.hostSavePwd;
  $('hostPwd').placeholder = !state.hostSavePwd
    ? '連線時詢問'
    : (hadPwd ? '留空維持原密碼' : '');
}

function openHostModal(host) {
  $('hostModalTitle').textContent = host ? '編輯主機' : '新增主機';
  $('hostId').value = host ? host.id : '';
  $('hostName').value = host ? host.name : '';
  $('hostAddr').value = host ? host.address : '';
  $('hostPort').value = host && host.port && host.port !== 3389 ? host.port : '';
  $('hostGroup').value = host && host.group !== '預設' ? host.group : '';
  $('hostUser').value = host
    ? (host.domain && host.username ? `${host.domain}\\${host.username}` : host.username)
    : '';
  $('hostPwd').value = '';

  const preset = host ? presetForSize(host.width, host.height) : 'auto';
  $('hostResPreset').value = preset;
  $('hostCustomResRow').classList.toggle('hidden', preset !== 'custom');
  if (preset === 'custom') {
    $('hostCustomW').value = host.width;
    $('hostCustomH').value = host.height;
  }

  const shareFolder = host?.shared_folder || '';
  const matchPreset = state.sharePresets.find((p) => p.path === shareFolder);
  if (!shareFolder) {
    $('hostSharePreset').value = '';
    $('hostSharePath').value = '';
    $('hostSharePathWrap').classList.add('hidden');
  } else if (matchPreset) {
    $('hostSharePreset').value = matchPreset.path;
    $('hostSharePath').value = shareFolder;
    $('hostSharePathWrap').classList.add('hidden');
  } else {
    $('hostSharePreset').value = '__custom__';
    $('hostSharePath').value = shareFolder;
    $('hostSharePathWrap').classList.remove('hidden');
  }

  state.hostSavePwd = host ? host.save_password : true;
  state.hostClip = host ? host.clipboard_enabled : true;
  state.hostHook = host ? host.keyboard_hook_enabled : true;
  state.hostAudio = host ? (host.audio_enabled ?? true) : true;
  syncHostPwdToggle(Boolean(host?.has_password));
  $('toggleHostClip').classList.toggle('on', state.hostClip);
  $('toggleHostHook').classList.toggle('on', state.hostHook);
  $('toggleHostAudio').classList.toggle('on', state.hostAudio);

  $('hostModal').classList.add('open');
  setModalOpenFlag(true);
  $('hostAddr').focus();
}

async function saveHostModal() {
  const address = $('hostAddr').value.trim();
  if (!address) { notify('請填寫位址', true); return; }

  const portRaw = $('hostPort').value.trim();
  const portParsed = portRaw ? parseInt(portRaw, 10) : 3389;
  const port = portParsed > 0 && portParsed <= 65535 ? portParsed : 3389;

  const presetVal = $('hostResPreset').value;
  let width = 0;
  let height = 0;
  if (presetVal === 'custom') {
    width = Math.max(640, Math.min(4096, parseInt($('hostCustomW').value, 10) || 1920)) & ~1;
    height = Math.max(480, Math.min(2160, parseInt($('hostCustomH').value, 10) || 1080)) & ~1;
  } else {
    const p = RESOLUTION_PRESETS.find((x) => x.value === presetVal);
    if (p) { width = p.w; height = p.h; }
  }

  const sharePresetVal = $('hostSharePreset').value;
  const sharedFolder = sharePresetVal === '__custom__'
    ? $('hostSharePath').value.trim()
    : sharePresetVal;

  try {
    const data = await invoke('vault_save_host', {
      input: {
        id: $('hostId').value || null,
        name: $('hostName').value.trim(),
        address,
        port,
        group: $('hostGroup').value.trim(),
        username: $('hostUser').value.trim(),
        domain: '',
        save_password: state.hostSavePwd,
        password: state.hostSavePwd ? $('hostPwd').value : null,
        width,
        height,
        clipboard_enabled: state.hostClip,
        keyboard_hook_enabled: state.hostHook,
        audio_enabled: state.hostAudio,
        shared_folder: sharedFolder
      }
    });
    applyVaultData(data);
    scheduleAutoSync();
    $('hostModal').classList.remove('open');
    setModalOpenFlag(false);
    notify('已儲存');
  } catch (e) { notify(String(e), true); }
}

function openPromptModal(cfg) {
  state.pendingPrompt = cfg;
  $('promptTitle').textContent = cfg.title;
  const errEl = $('promptErr');
  errEl.textContent = cfg.errorMsg || '';
  errEl.classList.toggle('hidden', !cfg.errorMsg);
  $('promptUserWrap').classList.toggle('hidden', !cfg.needUser);
  $('promptUser').value = cfg.defaultUser || '';
  $('promptPwd').value = '';
  state.promptUpdatePwd = true;
  $('togglePromptUpdate').classList.toggle('on', true);
  $('promptUpdateWrap').classList.toggle('hidden', !cfg.canUpdateSaved);
  $('promptModal').classList.add('open');
  setModalOpenFlag(true);
  setTimeout(() => (cfg.needUser && !cfg.defaultUser ? $('promptUser') : $('promptPwd')).focus(), 30);
}

function submitPromptModal() {
  if (!state.pendingPrompt) return;
  const cfg = state.pendingPrompt;
  const username = $('promptUser').value.trim();
  const password = $('promptPwd').value;
  if (cfg.needUser && !username) { notify('請輸入帳號', true); return; }
  $('promptModal').classList.remove('open');
  setModalOpenFlag(false);
  state.pendingPrompt = null;
  cfg.onSubmit({ username, password, updateSaved: cfg.canUpdateSaved && state.promptUpdatePwd });
}

async function exportVault() {
  try {
    const json = await invoke('vault_export_json');
    const blob = new Blob([json], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = 'mist-rdp.vault.json';
    a.click();
    URL.revokeObjectURL(url);
    notify('已匯出');
  } catch (e) { notify(String(e), true); }
}

async function confirmImportVault() {
  const file = $('importFile').files?.[0];
  const pwd = $('importPwd').value;
  if (!file || !pwd) { notify('請選擇檔案並輸入主密碼', true); return; }
  try {
    const jsonStr = await file.text();
    const data = await invoke('vault_import_json', { jsonStr, masterPassword: pwd });
    applyVaultData(data);
    $('importModal').classList.remove('open');
    setModalOpenFlag(false);
    $('importPwd').value = '';
    notify('已匯入');
  } catch (e) { notify(String(e), true); }
}

function escapeHtml(s) {
  return String(s ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}


// ── Zero-Knowledge Vault Sync ─────────────────────────────────────────────────

let autoSyncTimer = null;

function populateSyncFolderPresets() {
  const sel = $('syncFolderPreset');
  if (!sel) return;
  const opts = ['<option value="__custom__">自訂資料夾路徑...</option>'];
  for (const p of state.syncPresets) {
    opts.push(`<option value="${escapeHtml(p.path)}">${escapeHtml(p.label)}</option>`);
  }
  sel.innerHTML = opts.join('');
}

function syncFormVisibility(mode) {
  $('syncFolderWrap').classList.toggle('hidden', mode !== 'folder');
  $('syncWebDavWrap').classList.toggle('hidden', mode !== 'webdav');
  $('syncAutoRow').classList.toggle('hidden', mode === 'disabled');
}

function formatSyncTime(ts) {
  if (!ts) return '';
  const d = new Date(ts);
  const hh = String(d.getHours()).padStart(2, '0');
  const mm = String(d.getMinutes()).padStart(2, '0');
  return `${hh}:${mm}`;
}

function renderSyncConfig() {
  const cfg = state.syncConfig;
  if (!cfg) return;
  $('syncMode').value = cfg.mode || 'disabled';
  $('syncFolderPath').value = cfg.folder_path || '';
  const matchedPreset = state.syncPresets.find((p) => p.path === cfg.folder_path);
  $('syncFolderPreset').value = matchedPreset ? matchedPreset.path : '__custom__';
  $('syncDavUrl').value = cfg.webdav_url || '';
  $('syncDavFile').value = cfg.webdav_file_name || 'mist-rdp.vault.json';
  $('syncDavUser').value = cfg.webdav_username || '';
  $('syncDavPwd').value = '';
  $('syncDavPwd').placeholder = cfg.has_webdav_password ? '已儲存（留空維持原密碼）' : '密碼';
  $('btnSyncClearPwd').classList.toggle('hidden', !cfg.has_webdav_password);

  state.syncDavInsecure = Boolean(cfg.webdav_allow_invalid_certs);
  $('toggleSyncDavInsecure').classList.toggle('on', state.syncDavInsecure);
  state.syncAuto = cfg.auto_sync !== false;
  $('toggleSyncAuto').classList.toggle('on', state.syncAuto);

  syncFormVisibility(cfg.mode || 'disabled');

  const tag = $('syncStatusTag');
  if (state.syncInFlight) {
    tag.textContent = '同步中...';
    tag.classList.add('accent');
  } else if (!cfg.mode || cfg.mode === 'disabled') {
    tag.textContent = '未啟用';
    tag.classList.remove('accent');
  } else if (cfg.last_sync_at) {
    tag.textContent = `${formatSyncTime(cfg.last_sync_at)} · ${cfg.last_sync_status || '已同步'}`;
    tag.classList.add('accent');
  } else {
    tag.textContent = cfg.mode === 'folder' ? '資料夾待同步' : 'WebDAV 待同步';
    tag.classList.add('accent');
  }

  $('btnSidebarSync').classList.toggle('hidden', !cfg.mode || cfg.mode === 'disabled');
}

function collectSyncFormInput() {
  return {
    mode: $('syncMode').value,
    folder_path: $('syncFolderPath').value.trim(),
    webdav_url: $('syncDavUrl').value.trim(),
    webdav_username: $('syncDavUser').value.trim(),
    webdav_file_name: $('syncDavFile').value.trim() || 'mist-rdp.vault.json',
    webdav_allow_invalid_certs: state.syncDavInsecure,
    webdav_password: $('syncDavPwd').value || null,
    clear_webdav_password: false,
    auto_sync: state.syncAuto
  };
}

async function saveSyncConfigUi(opts = {}) {
  try {
    const cfg = await invoke('sync_save_config', { input: collectSyncFormInput() });
    state.syncConfig = cfg;
    renderSyncConfig();
    if (!opts.quiet) notify('同步設定已儲存');
    return true;
  } catch (e) {
    notify(String(e), true);
    return false;
  }
}

async function testSyncConnectionUi() {
  const btn = $('btnSyncTest');
  btn.disabled = true;
  try {
    const msg = await invoke('sync_test_connection', { input: collectSyncFormInput() });
    notify(msg);
  } catch (e) {
    notify(String(e), true);
  } finally {
    btn.disabled = false;
  }
}

async function runSyncNow(opts = {}) {
  if (!state.vaultStatus.unlocked || state.syncInFlight) return;
  if (!state.syncConfig || state.syncConfig.mode === 'disabled') {
    if (!opts.silent) notify('尚未啟用同步', true);
    return;
  }
  state.syncInFlight = true;
  renderSyncConfig();
  $('btnSyncNow').disabled = true;
  $('btnSidebarSync').disabled = true;
  try {
    const res = await invoke('sync_now');
    state.syncConfig = res.config;
    applyVaultData(res.vault_data);
    if (!opts.silent || res.added > 0 || res.updated > 0 || res.deleted > 0) {
      notify(res.summary);
    }
  } catch (e) {
    if (!opts.silent) notify(String(e), true);
  } finally {
    state.syncInFlight = false;
    $('btnSyncNow').disabled = false;
    $('btnSidebarSync').disabled = false;
    renderSyncConfig();
  }
}

function maybeAutoSyncOnUnlock() {
  if (state.syncConfig && state.syncConfig.mode !== 'disabled' && state.syncConfig.auto_sync) {
    runSyncNow({ silent: true });
  }
}

function scheduleAutoSync() {
  clearTimeout(autoSyncTimer);
  if (!state.vaultStatus.unlocked) return;
  if (!state.syncConfig || state.syncConfig.mode === 'disabled' || !state.syncConfig.auto_sync) return;
  autoSyncTimer = setTimeout(() => runSyncNow({ silent: true }), 1500);
}

bootstrap();
