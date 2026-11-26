const appGate = { configured:false, locked:false, idle_minutes:0 };
let lastAppInput = Date.now();
let appLockPending = false;

function markAppActivity() { lastAppInput = Date.now(); }

function updateAppSecurity(status) {
  Object.assign(appGate,status);
  markAppActivity();
  $('appPasswordSummary').textContent = appGate.configured ? '更換 App 主密碼' : '設定 App 主密碼';
  $('appCurrentPasswordRow').classList.toggle('hidden',!appGate.configured);
  $('appCurrentPassword').required = appGate.configured;
  $('appCurrentPassword').disabled = !appGate.configured;
  $('appIdleMinutes').value = appGate.idle_minutes;
  $('appIdleMinutes').disabled = !appGate.configured;
  $('btnAppIdleSave').disabled = !appGate.configured;
  renderVaultGate();
}

function renderAppLock() {
  $('viewAppLock').classList.toggle('hidden',!appGate.locked);
  $('tabStrip').classList.toggle('hidden',appGate.locked);
  $('btnAddTab').classList.toggle('hidden',appGate.locked);
  if (appGate.locked) $('btnTitlebarHook').classList.add('hidden');
}

async function lockApp(auto = false) {
  if (appGate.locked || appLockPending || (auto && state.sessions.size)) return;
  if (!appGate.configured) {
    state.activeNav = 'settings';
    state.activeVault = null;
    state.activeGroup = null;
    renderVaultWorkspace();
    $('appPasswordSettings').open = true;
    $('appNewPassword').focus();
    return;
  }
  appLockPending = true;
  try {
    const status = await invoke('app_lock');
    document.querySelectorAll('dialog[open]').forEach(dialog => dialog.close());
    document.querySelectorAll('.modal.open').forEach(modal => modal.classList.remove('open'));
    document.querySelectorAll('input[type=password]').forEach(field => { field.value = ''; });
    state.pendingPrompt = null;
    $('appUnlockFeedback').textContent = '';
    updateAppSecurity(status);
    $('appUnlockPassword').focus();
  } catch (error) {
    markAppActivity();
    notify(String(error),true);
  } finally { appLockPending = false; }
}

function wireAppSecurity() {
  for (const event of ['pointermove','pointerdown','keydown','wheel']) {
    document.addEventListener(event, markAppActivity, { passive:true });
  }
  if (invoke) setInterval(() => {
    const elapsed = Date.now() - lastAppInput;
    if (appGate.configured && appGate.idle_minutes && elapsed >= appGate.idle_minutes * 60000) lockApp(true);
  },1000);
  $('btnAppLock').onclick = () => lockApp();

  $('appUnlockForm').onsubmit = async (event) => {
    event.preventDefault();
    const button = $('btnAppUnlock');
    if (button.disabled) return;
    const field = $('appUnlockPassword');
    const password = field.value;
    field.value = '';
    button.disabled = true;
    $('appUnlockFeedback').textContent = '';
    try {
      updateAppSecurity(await invoke('app_unlock',{password}));
      setModalOpenFlag(false);
      await loadWorkspace();
      if (!$('viewVaultGate').classList.contains('hidden')) $('gatePwd').focus();
    } catch (error) {
      $('appUnlockFeedback').textContent = String(error);
      if (appGate.locked) field.focus();
      else notify(String(error),true);
    } finally { button.disabled = false; }
  };

  $('appPasswordForm').onsubmit = async (event) => {
    event.preventDefault();
    const button = $('btnAppPasswordSave');
    if (button.disabled) return;
    const fields = ['appCurrentPassword','appNewPassword','appNewPasswordConfirm'].map($);
    const [current,next,confirm] = fields.map(field => field.value);
    const feedback = $('appSettingsFeedback');
    if (next !== confirm) { feedback.textContent = '兩次輸入的 App 主密碼不同'; return; }
    button.disabled = true;
    try {
      updateAppSecurity(await invoke('app_security_password',{current,next}));
      fields.forEach(field => { field.value = ''; });
      $('appPasswordSettings').open = false;
      feedback.textContent = '已儲存 App 主密碼';
    } catch (error) { feedback.textContent = String(error); }
    finally { button.disabled = false; }
  };

  $('appIdleForm').onsubmit = async (event) => {
    event.preventDefault();
    const feedback = $('appSettingsFeedback');
    const button = $('btnAppIdleSave');
    if (button.disabled) return;
    button.disabled = true;
    try {
      updateAppSecurity(await invoke('app_security_idle',{minutes:$('appIdleMinutes').valueAsNumber}));
      feedback.textContent = '已儲存自動鎖定設定';
    } catch (error) { feedback.textContent = String(error); }
    finally { button.disabled = !appGate.configured; }
  };
}
