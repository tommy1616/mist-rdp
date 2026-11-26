const invoke = window.__TAURI__?.core?.invoke;

const RESOLUTION_PRESETS = [
  { value: 'auto', get label() { return tr('auto'); }, w: 0, h: 0 },
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
  vaults: new Map(),
  collapsedVaults: new Set(),
  activeVault: null,
  settingsVault: 'local',
  gateVisible: false,
  workspaceLoaded: false,
  sharePresets: [],
  activeNav: 'hosts',
  activeGroup: null,
  tabs: [{ id: 'home', type: 'home', title: tr('hosts') }],
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
  $('hostResPreset').innerHTML = baseOpts + `<option value="custom">${tr('custom')}</option>`;
}

function populateSharePresets() {
  const opts = [`<option value="">${tr('notShared')}</option>`];
  for (const p of state.sharePresets) {
    opts.push(`<option value="${escapeHtml(p.path)}">${escapeHtml(p.label)}</option>`);
  }
  opts.push(`<option value="__custom__">${tr('customPath')}</option>`);
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

// Desktop scale (percent) sent to the server. In auto-size mode the remote desktop is as large as
// the window in physical pixels, so on a HiDPI display (Retina, 150% Windows) the server must be
// told to draw its UI at that scale, as mstsc / Windows App do; otherwise everything looks tiny.
// Fixed resolutions stay at 100%.
function desktopScaleFor(sess) {
  if (sess && (sess.requestedW || sess.requestedH)) return 100;
  const pct = Math.round(((window.devicePixelRatio || 1) * 100) / 25) * 25;
  return Math.max(100, Math.min(500, pct));
}

function resolveRequestedSize(w, h) {
  if (w >= 640 && h >= 480) {
    return [Math.min(4096, w) & ~1, Math.min(2160, h) & ~1];
  }
  return calcAutoResolution();
}

function resolutionLabel(w, h) {
  return w >= 640 && h >= 480 ? `${w}×${h}` : tr('auto');
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
  if (titleLabel) titleLabel.textContent = on ? tr('shortcutsRemote') : tr('hotkeyLocal');
  if (titleBtn) {
    titleBtn.title = on
      ? tr('systemShortcutsAltTab')
      : tr('hotkeyLocalTip');
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
    ? tr('lossless32BitQuality')
    : tr('lossyTip');
}

function syncAudioToggleUi(sess) {
  const btn = $('btnHudAudio');
  const icon = $('hudAudioIcon');
  const text = $('hudAudioText');
  if (!btn || !sess) return;
  const on = Boolean(sess.audioEnabled);
  btn.classList.toggle('on', on);
  btn.title = on ? tr('soundOnTip') : tr('soundOffTip');
  if (icon) icon.innerHTML = on ? AUDIO_SVG_ON : AUDIO_SVG_OFF;
  if (text) text.textContent = on ? tr('sound') : tr('muted');
}

async function toggleActiveLosslessMode() {
  const sess = state.sessions.get(state.activeTabId);
  if (!sess) return;
  sess.losslessQuality = !sess.losslessQuality;
  syncLosslessToggleUi(sess);

  // Persist as default preference in vault settings
  const owner = getVault(sess.params.vault_id || LOCAL_VAULT);
  if (owner?.data) {
    try {
      const next = { ...owner.data.settings, lossless_quality: sess.losslessQuality };
      const data = await invoke('vault_update_settings', { ...vaultArgs(owner.id), settings: next });
      applyVaultData(data,owner.id);
    } catch (_) {}
  }

  notify(sess.losslessQuality ? tr('losslessOn') : tr('lossyOn'));
  await reconnectSessionInPlace(sess);
}

function toggleActiveAudio() {
  const sess = state.sessions.get(state.activeTabId);
  if (!sess) return;
  sess.audioEnabled = !sess.audioEnabled;
  invoke?.('rdp_set_audio_enabled', { sessionId: sess.id, enabled: sess.audioEnabled });
  syncAudioToggleUi(sess);
  notify(sess.audioEnabled ? tr('remoteSoundOn') : tr('remoteSoundOff'));
  setTimeout(() => sess.canvas?.focus(), 10);
}

function toggleActiveHookMode() {
  const sess = state.sessions.get(state.activeTabId);
  if (!sess) return;
  sess.hookEnabled = !sess.hookEnabled;
  invoke?.('set_keyboard_hook', { enabled: sess.hookEnabled });
  syncHookToggleUi(sess);
  notify(sess.hookEnabled ? tr('hotkeysRemoteNow') : tr('hotkeysLocalNow'));
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
  $('btnHudPin').title = state.hudPinned ? tr('hudPinned') : tr('hudAuto');
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
  invoke?.('rdp_resize', { sessionId: sess.id, width: w, height: h, scale: desktopScaleFor(sess) });
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
  wireAppSecurity();
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
      notify(enabled ? tr('hotkeysRemoteNow') : tr('hotkeysBackLocal'));
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
  updateAppSecurity(await invoke('app_security_status'));
  if (appGate.locked) $('appUnlockPassword').focus();
  else await loadWorkspace();
}

async function loadWorkspace() {
  if (state.workspaceLoaded) return;
  const [info, presets, profiles] = await Promise.all([
    invoke('get_bootstrap_info'),
    invoke('get_default_share_folders').catch(() => []),
    invoke('webdav_profiles')
  ]);
  state.platform = info.platform;
  state.wsPort = info.ws_port;
  state.wsToken = info.ws_token;
  state.sharePresets = Array.isArray(presets) ? presets : [];
  populateSharePresets();
  connectStreamWs();
  renderTabs();
  await initializeVaults(info.vault_status, profiles);
  state.workspaceLoaded = true;
  if (!state.updateUiReady) { state.updateUiReady = true; initUpdateUi(); }
  renderVaultGate();
}

const wsTextDecoder = new TextDecoder();
const wsTextEncoder = new TextEncoder();
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

// Keyboard input goes over the ordered loopback socket (IPC calls can reorder or stall).
function sendInputFrame(kind, sid, body) {
  const ws = state.ws;
  if (!ws || ws.readyState !== WebSocket.OPEN) return false;
  const sidBytes = wsTextEncoder.encode(sid);
  const frame = new Uint8Array(3 + sidBytes.length + body.length);
  frame[0] = kind;
  frame[1] = sidBytes.length & 0xff;
  frame[2] = sidBytes.length >> 8;
  frame.set(sidBytes, 3);
  frame.set(body, 3 + sidBytes.length);
  ws.send(frame);
  return true;
}

function sendKey(sid, scancode, extended, pressed) {
  const flags = (extended ? 1 : 0) | (pressed ? 2 : 0);
  if (!sendInputFrame(0x10, sid, [scancode, flags])) {
    invoke?.('rdp_key', { sessionId: sid, scancode, extended, pressed });
  }
}

function sendUnicode(sid, codeUnit, pressed) {
  if (!sendInputFrame(0x11, sid, [codeUnit & 0xff, codeUnit >> 8, pressed ? 1 : 0])) {
    invoke?.('rdp_unicode', { sessionId: sid, codeUnit, pressed });
  }
}

const MOUSE_ACTIONS = { move: 0, down: 1, up: 2, wheel: 3, hwheel: 4 };
function sendMouse(sid, x, y, button, action, wheelDelta) {
  const d = wheelDelta & 0xffff;
  if (!sendInputFrame(0x12, sid, [x & 0xff, x >> 8, y & 0xff, y >> 8, button, MOUSE_ACTIONS[action], d & 0xff, d >> 8])) {
    invoke?.('rdp_mouse', { sessionId: sid, x, y, button, action, wheelDelta });
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
    notify(payload.message || tr('badCredentials'), true);
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
  $('btnHudFullscreen').title = state.isFullscreen ? tr('exitFullscreen') : tr('fullscreen');
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


}

function selectTab(id) {
  if (appGate.locked) return;
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
  renderVaultGate();

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
    sess.state === 'connected' ? `${sess.fbW}×${sess.fbH}${rtt}` : (sess.state === 'connecting' ? tr('connecting') : tr('disconnectedState'));

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
      ? tr('connecting2')
      : (isAuthError ? (sess.message || tr('badCredentials')) : tr('connectionLost'));
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
          vault_id: host.vault_id,
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
    vault_id: host.vault_id,
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
    losslessQuality: opts.lossless_quality ?? (getVault(opts.vault_id)?.data?.settings.lossless_quality ?? currentPreferences().lossless_quality),
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
        vault_id: vaultArgs(opts.vault_id || LOCAL_VAULT).vaultId,
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
        shared_folder: sess.sharedFolder || null,
        scale: desktopScaleFor(sess)
      }
    });
  } catch (err) {
    sess.state = 'error';
    const msg = String(err);
    sess.code = (msg === 'NEED_PASSWORD' || msg === 'NEED_CREDENTIALS') ? 'AUTH_FAILED' : 'ERROR';
    sess.message = msg === 'NEED_PASSWORD' ? tr('enterPassword') : msg;
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
  if (!state.sessions.has(sid)) return;
  markAppActivity();
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
    sendMouse(sid, x, y, 0, 'move', 0);
  });

  canvas.addEventListener('mousedown', (e) => {
    e.preventDefault();
    canvas.focus();
    const [x, y] = mapCoords(e);
    sendMouse(sid, x, y, e.button, 'down', 0);
  });

  canvas.addEventListener('mouseup', (e) => {
    e.preventDefault();
    const [x, y] = mapCoords(e);
    sendMouse(sid, x, y, e.button, 'up', 0);
  });

  canvas.addEventListener('wheel', (e) => {
    e.preventDefault();
    const [x, y] = mapCoords(e);
    if (Math.abs(e.deltaY) >= Math.abs(e.deltaX)) {
      const delta = e.deltaY > 0 ? -120 : 120;
      sendMouse(sid, x, y, 0, 'wheel', delta);
    } else if (e.deltaX !== 0) {
      const delta = e.deltaX > 0 ? 120 : -120;
      sendMouse(sid, x, y, 0, 'hwheel', delta);
    }
  }, { passive: false });

  const resolveScancode = (e) => {
    const code = e.code || '';
    if (state.platform === 'macos' && (getVault(state.sessions.get(sid)?.params.vault_id)?.data?.settings || currentPreferences()).mac_cmd_to_ctrl) {
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
    sendKey(sid, sc[0], sc[1], true);
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
    sendKey(sid, sc[0], sc[1], false);
  });

  canvas.addEventListener('compositionend', (e) => {
    const text = e.data || '';
    for (let i = 0; i < text.length; i++) {
      const cu = text.charCodeAt(i);
      sendUnicode(sid, cu, true);
      sendUnicode(sid, cu, false);
    }
  });
}

// ── UI Event Wiring ───────────────────────────────────────────────────────────

function wireUiEvents() {
  wireVaultUi();
  const tabs = $('tabStrip');
  tabs.onclick = (event) => {
    const close = event.target.closest('[data-close-tab]');
    const tab = event.target.closest('[data-tab]');
    if (close) closeRdpTab(close.dataset.closeTab);
    else if (tab) selectTab(tab.dataset.tab);
  };
  const middleTab = (event) => {
    const id = event.target.closest('[data-tab]')?.dataset.tab;
    return event.button === 1 && state.sessions.has(id) ? id : null;
  };
  tabs.onmousedown = (event) => { if (middleTab(event)) event.preventDefault(); };
  tabs.onauxclick = (event) => {
    const id = middleTab(event);
    if (id) {
      event.preventDefault();
      closeRdpTab(id);
    }
  };

  $('btnGateImport').onclick = () => { $('importModal').classList.add('open'); setModalOpenFlag(true); };

  document.querySelectorAll('[data-nav]').forEach((el) => {
    el.onclick = () => {
      state.activeNav = el.dataset.nav;
      state.activeVault = null;
      state.activeGroup = null;
      renderVaultWorkspace();
    };
  });

  $('btnNewGroup').onclick = () => openGroupDialog('create');
  $('btnRenameGroup').onclick = () => openGroupDialog('rename');
  $('btnDeleteGroup').onclick = () => openGroupDialog('delete');
  $('btnGroupCancel').onclick = () => $('groupDialog').close();
  $('groupDialog').oncancel = (e) => { if ($('btnGroupSubmit').disabled) e.preventDefault(); };
  $('groupDialog').onclose = () => setModalOpenFlag(false);
  $('groupForm').onsubmit = async (e) => {
    e.preventDefault();
    const dialog = $('groupDialog');
    const btn = $('btnGroupSubmit');
    const err = $('groupError');
    if (btn.disabled) return;
    err.classList.add('hidden');
    btn.disabled = true;
    $('btnGroupCancel').disabled = true;
    try {
      const vaultId = dialog.dataset.vault;
      const data = await invoke('vault_change_group', {
        ...vaultArgs(vaultId),
        id: dialog.dataset.id || null,
        name: dialog.dataset.action === 'delete' ? null : $('groupName').value
      });
      applyVaultData(data,vaultId);
      dialog.close();
    } catch (error) {
      err.textContent = String(error);
      err.classList.remove('hidden');
    } finally {
      btn.disabled = false;
      $('btnGroupCancel').disabled = false;
    }
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
    invoke?.('rdp_resize', { sessionId: sess.id, width: w, height: h, scale: desktopScaleFor(sess) });
    notify(tr('resolutionValue', { w, h }));
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
      ? hostById(params.vault_id || LOCAL_VAULT,params.host_id)
      : null;

    openPromptModal({
      title: params.title,
      errorMsg: sess.message || tr('badCredentials'),
      needUser: true,
      defaultUser: params.username || savedHost?.username || '',
      canUpdateSaved: Boolean(savedHost && savedHost.save_password),
      onSubmit: async ({ username, password, updateSaved }) => {
        if (updateSaved && savedHost) {
          try {
            const data = await invoke('vault_update_host_credentials', {
              ...vaultArgs(params.vault_id || LOCAL_VAULT),
              hostId: savedHost.id,
              username: username || null,
              password
            });
            applyVaultData(data,params.vault_id || LOCAL_VAULT);
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
    n.classList.toggle('active', n.dataset.nav === state.activeNav && !state.activeVault);
  });
  $('panelHosts').classList.toggle('hidden', state.activeNav !== 'hosts');
  $('panelSettings').classList.toggle('hidden', state.activeNav !== 'settings');
}

function doQuickConnect() {
  const address = $('quickAddr').value.trim();
  if (!address) { notify(tr('enterHostAddress'), true); return; }
  const portVal = parseInt($('quickPort').value.trim(), 10);
  const port = portVal > 0 && portVal <= 65535 ? portVal : 3389;
  const preset = RESOLUTION_PRESETS.find((p) => p.value === $('quickRes').value);

  initiateHostConnect({
    id: 'quick',
    name: address,
    address,
    port,
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
    ? tr('askOnConnect')
    : (hadPwd ? tr('keepPassword') : '');
}

function openGroupDialog(action) {
  const vault = getVault(state.activeVault);
  const group = vault?.data?.groups.find(g => g.id === state.activeGroup);
  if (!vault?.data || (action !== 'create' && !group)) return;
  const dialog = $('groupDialog');
  const deleting = action === 'delete';
  dialog.dataset.vault = vault.id;
  dialog.dataset.id = action === 'create' ? '' : group.id;
  dialog.dataset.action = action;
  $('groupDialogTitle').textContent = { create: tr('newGroup'), rename: tr('rename'), delete: tr('deleteGroup') }[action];
  $('groupNameField').classList.toggle('hidden', deleting);
  $('groupName').disabled = deleting;
  $('groupName').value = action === 'rename' ? group.name : '';
  $('groupDeleteInfo').classList.toggle('hidden', !deleting);
  $('groupDeleteInfo').textContent = deleting ? tr('deleteGroupInfo', { name: group.name }) : '';
  $('btnGroupSubmit').textContent = deleting ? tr('deleteGroup') : tr('save');
  $('groupError').classList.add('hidden');
  dialog.showModal();
  setModalOpenFlag(true);
  (deleting ? $('btnGroupCancel') : $('groupName')).focus();
}

function openHostModal(host) {
  $('hostModalTitle').textContent = host ? tr('editHost') : tr('newHost');
  $('hostId').value = host ? host.id : '';
  $('hostName').value = host ? host.name : '';
  $('hostAddr').value = host ? host.address : '';
  $('hostPort').value = host && host.port && host.port !== 3389 ? host.port : '';
  const vaults = unlockedVaults();
  const picker = $('hostVault');
  const current = host?.vault_id || (getVault(state.activeVault)?.data ? state.activeVault : vaults[0]?.id);
  picker.replaceChildren(...vaults.map(v => new Option(v.name,v.id)));
  picker.value = current || '';
  picker.disabled = Boolean(host);
  updateHostGroupOptions(picker.value,host ? host.group_id : state.activeGroup);
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
  if (!address) { notify(tr('enterAddress'), true); return; }

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
    const vaultId = $('hostVault').value;
    if (!getVault(vaultId)?.data) throw new Error(tr('unlockTargetFirst'));
    const data = await invoke('vault_save_host', {
      ...vaultArgs(vaultId),
      input: {
        id: $('hostId').value || null,
        name: $('hostName').value.trim(),
        address,
        port,
        group_id: $('hostGroup').value || null,
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
    applyVaultData(data,vaultId);
    $('hostModal').classList.remove('open');
    setModalOpenFlag(false);
    notify(tr('saved'));
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
  if (cfg.needUser && !username) { notify(tr('enterUsername'), true); return; }
  $('promptModal').classList.remove('open');
  setModalOpenFlag(false);
  state.pendingPrompt = null;
  cfg.onSubmit({ username, password, updateSaved: cfg.canUpdateSaved && state.promptUpdatePwd });
}

async function exportVault() {
  try {
    const json = await invoke('vault_export_json',vaultArgs(settingsOwner()?.id || LOCAL_VAULT));
    const blob = new Blob([json], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = 'mist-rdp.vault.json';
    a.click();
    URL.revokeObjectURL(url);
    notify(tr('exported'));
  } catch (e) { notify(String(e), true); }
}

async function confirmImportVault() {
  const file = $('importFile').files?.[0];
  const pwd = $('importPwd').value;
  if (!file || !pwd) { notify(tr('chooseFileAndPassword'), true); return; }
  try {
    const jsonStr = await file.text();
    const data = await invoke('vault_import_json', { jsonStr, masterPassword: pwd });
    state.gateVisible = false;
    applyVaultData(data);
    $('importModal').classList.remove('open');
    setModalOpenFlag(false);
    $('importPwd').value = '';
    notify(tr('imported'));
  } catch (e) { notify(String(e), true); }
}

function escapeHtml(s) {
  return String(s ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}


window.addEventListener('i18n-changed', () => {
  const keep = ['quickRes', 'hudResSelect', 'hostResPreset', 'hostSharePreset'].map((id) => [id, $(id).value]);
  state.tabs[0].title = tr('hosts');
  const localVault = state.vaults.get(LOCAL_VAULT);
  if (localVault) localVault.name = tr('local');
  populateResolutionSelects();
  populateSharePresets();
  keep.forEach(([id, v]) => { $(id).value = v; });
  renderTabs();
  updateAppSecurity({});
  renderVaultWorkspace();
  const sess = state.sessions.get(state.activeTabId);
  if (sess) renderActiveRdpChrome(sess);
});

bootstrap().catch(error => notify(String(error),true));

// ---- Self-update (GitHub Releases) ----
const AUTO_UPDATE_KEY = 'mistrdp.autoUpdateCheck';
let updateBusy = false;
function setUpdateStatus(text, danger = false) {
  const el = $('updateStatus');
  el.textContent = text;
  el.classList.toggle('danger', danger);
}
async function checkForUpdate(silent) {
  if (!invoke || updateBusy) return;
  state.updateConfirmed = false;
  if (!silent) setUpdateStatus(tr('updateChecking'));
  try {
    const info = await invoke('update_check');
    state.updateInfo = info;
    $('btnInstallUpdate').classList.toggle('hidden', !(info.available && info.installable));
    if (!info.available) {
      setUpdateStatus(tr('updateLatest', { current: info.current }));
    } else if (!info.installable) {
      setUpdateStatus(tr('updateNotInstallable', { latest: info.latest }));
    } else {
      setUpdateStatus(tr('updateAvailable', { latest: info.latest, current: info.current }));
    }
    if (silent && info.available) notify(tr('updateToast', { latest: info.latest }));
  } catch (err) {
    if (!silent) setUpdateStatus(tr('updateCheckFailed', { error: err }), true);
  }
}
async function installUpdate() {
  if (!invoke || updateBusy || !state.updateInfo?.available) return;
  if (state.sessions.size > 0 && !state.updateConfirmed) {
    state.updateConfirmed = true;
    setUpdateStatus(tr('updateConfirmSessions'), true);
    return;
  }
  updateBusy = true;
  $('btnInstallUpdate').disabled = true;
  $('btnCheckUpdate').disabled = true;
  $('updateBar').classList.remove('hidden');
  $('updateBarFill').style.width = '0%';
  setUpdateStatus(tr('updateDownloading', { percent: 0 }));
  try {
    await invoke('update_install');
  } catch (err) {
    updateBusy = false;
    state.updateConfirmed = false;
    $('btnInstallUpdate').disabled = false;
    $('btnCheckUpdate').disabled = false;
    $('updateBar').classList.add('hidden');
    setUpdateStatus(tr('updateFailed', { error: err }), true);
  }
}
function initUpdateUi() {
  const auto = $('chkAutoUpdateCheck');
  auto.checked = localStorage.getItem(AUTO_UPDATE_KEY) !== '0';
  auto.addEventListener('change', () => localStorage.setItem(AUTO_UPDATE_KEY, auto.checked ? '1' : '0'));
  $('btnCheckUpdate').addEventListener('click', () => checkForUpdate(false));
  $('btnInstallUpdate').addEventListener('click', installUpdate);
  if (window.__TAURI__?.event?.listen) {
    window.__TAURI__.event.listen('update-progress', (ev) => {
      const p = ev.payload || {};
      if (p.stage === 'applying') {
        $('updateBarFill').style.width = '100%';
        setUpdateStatus(tr('updateApplying'));
      } else if (p.total) {
        const percent = Math.min(100, Math.floor((p.done / p.total) * 100));
        $('updateBarFill').style.width = percent + '%';
        setUpdateStatus(tr('updateDownloading', { percent }));
      }
    });
  }
  if (auto.checked) setTimeout(() => checkForUpdate(true), 3000);
}
