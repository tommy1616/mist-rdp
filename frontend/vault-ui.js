const LOCAL_VAULT = 'local';
const UNGROUPED = 'ungrouped';
const DEFAULT_VAULT_SETTINGS = { mac_cmd_to_ctrl: true, accept_self_signed_tls: true, lossless_quality: true };
const vaultArgs = (id) => ({ vaultId: id === LOCAL_VAULT ? null : id });
const getVault = (id) => state.vaults.get(id);
const unlockedVaults = () => [...state.vaults.values()].filter((vault) => vault.data);
const sortGroups = (vault) => [...(vault.data?.groups || [])].sort((a,b) => a.name.localeCompare(b.name, 'zh-TW'));
const hostById = (vaultId, id) => getVault(vaultId)?.data?.hosts.find((host) => host.id === id);

function currentPreferences() {
  return (getVault(LOCAL_VAULT)?.data || unlockedVaults()[0]?.data)?.settings || DEFAULT_VAULT_SETTINGS;
}

function settingsOwner() {
  return getVault(state.settingsVault)?.data ? getVault(state.settingsVault) : unlockedVaults()[0];
}

function applyVaultData(data, id = LOCAL_VAULT) {
  const vault = getVault(id);
  if (!vault) return;
  vault.data = data;
  Object.assign(vault.status, { exists:true, unlocked:true, vault_path:data.vault_path, remember_keychain:data.remember_keychain });
  renderVaultWorkspace();
}

function saveProfileView(profile) {
  const existing = getVault(profile.id);
  if (existing) Object.assign(existing,profile);
  else state.vaults.set(profile.id, profile);
}

async function initializeVaults(localStatus, profiles) {
  state.vaults.set(LOCAL_VAULT, { id:LOCAL_VAULT, name:'本機', status:localStatus });
  profiles.forEach(saveProfileView);
  await Promise.all([...state.vaults.values()].filter(v => v.status.exists).map(async vault => {
    const data = await invoke('vault_try_auto_unlock', vaultArgs(vault.id)).catch(() => null);
    if (data) applyVaultData(data,vault.id);
  }));
  if (!unlockedVaults().length) prepareVaultGate();
  renderVaultWorkspace();
}

function renderVaultGate() {
  const home = state.activeTabId === 'home';
  const unlocked = unlockedVaults().length > 0;
  renderAppLock();
  const showGate = state.workspaceLoaded && !appGate.locked && home && (!unlocked || state.gateVisible);
  $('viewVaultGate').classList.toggle('hidden', !showGate);
  $('viewHome').classList.toggle('hidden', appGate.locked || !home || !unlocked || showGate);
  $('viewRdp').classList.toggle('hidden', appGate.locked || home || !unlocked);
}

function openVaultGate(mode) {
  state.gateVisible = true;
  prepareVaultGate(mode);
  selectTab('home');
  $(isNewWebDav() ? 'gateDavName' : 'gatePwd').focus();
}

function prepareVaultGate(mode) {
  const select = $('gateMode');
  const locked = [...state.vaults.values()].filter(v => v.config && !v.data);
  select.replaceChildren(new Option('本機保管庫',LOCAL_VAULT),
    ...locked.map(v => new Option(`解鎖 ${v.name}`,v.id)),
    new Option('連線既有 WebDAV 保管庫','webdav'), new Option('建立 WebDAV 保管庫','create_webdav'));
  select.value = mode || (getVault(LOCAL_VAULT)?.status.exists ? LOCAL_VAULT : locked[0]?.id || LOCAL_VAULT);
  for (const id of ['gateDavName','gateDavUrl','gateDavUser','gateDavPwd','gateDavFile','gatePwd','gatePwdConfirm']) $(id).value = '';
  $('gateDavInsecure').checked = false;
  updateGateMode();
}

function isNewWebDav() { return ['webdav','create_webdav'].includes($('gateMode').value); }

function updateGateMode() {
  const mode = $('gateMode').value;
  const remote = isNewWebDav();
  const create = mode === 'create_webdav' || (mode === LOCAL_VAULT && !getVault(LOCAL_VAULT)?.status.exists);
  const name = getVault(mode)?.name;
  $('gateTitle').textContent = remote ? (create ? '建立 WebDAV 保管庫' : '連線既有 WebDAV 保管庫') : `${create ? '建立' : '解鎖'}${name || ''}保管庫`;
  $('btnGateSubmit').textContent = create ? '建立保管庫' : remote ? '連線並解鎖' : '解鎖';
  $('gateDavFields').classList.toggle('hidden', !remote);
  $('gateDavFields').disabled = !remote;
  $('gateConfirmWrap').classList.toggle('hidden', !create);
  $('gatePwdConfirm').disabled = !create;
  $('gatePwdConfirm').required = create;
  $('gatePwd').autocomplete = create ? 'new-password' : 'current-password';
  $('btnGateTest').classList.toggle('hidden', !remote);
  $('btnGateImport').classList.toggle('hidden', mode !== LOCAL_VAULT);
  $('btnGateBack').classList.toggle('hidden', !unlockedVaults().length);
  $('gateError').classList.add('hidden');
  $('gateFeedback').textContent = '';
  state.gateRemember = Boolean(getVault(mode)?.status.remember_keychain);
  $('toggleGateRemember').classList.toggle('on',state.gateRemember);
}

function gateConnectionInput() {
  return { mode:'webdav', webdav_url:$('gateDavUrl').value.trim(), webdav_username:$('gateDavUser').value.trim(),
    webdav_password:$('gateDavPwd').value || null, webdav_file_name:$('gateDavFile').value.trim(),
    webdav_allow_invalid_certs:$('gateDavInsecure').checked };
}

async function submitVaultGate(event) {
  event.preventDefault();
  const submit = $('btnGateSubmit');
  if (submit.disabled || $('btnGateTest').disabled) return;
  const mode = $('gateMode').value;
  let id = mode;
  const pass = $('gatePwd').value;
  const create = !$('gatePwdConfirm').disabled;
  const controls = [submit,$('gateMode'),$('btnGateImport'),$('btnGateBack'),$('btnGateTest')];
  $('gateError').classList.add('hidden');
  try {
    if (create && [...pass].length < 4) throw new Error('保管庫密碼至少 4 碼');
    if (create && pass !== $('gatePwdConfirm').value) throw new Error('兩次密碼不一致');
    controls.forEach(el => { el.disabled = true; });
    const credentials = { masterPassword:pass, rememberKeychain:state.gateRemember };
    let data;
    if (isNewWebDav()) {
      const result = await invoke('vault_webdav_login', { ...credentials, name:$('gateDavName').value.trim(), input:gateConnectionInput(), create });
      id = result.profile.id;
      saveProfileView(result.profile);
      data = result.vault_data;
    } else if (mode === LOCAL_VAULT) {
      data = await invoke(create ? 'vault_create' : 'vault_unlock', credentials);
    } else {
      data = await invoke('remote_vault_unlock', { ...credentials, vaultId:id });
    }
    state.gateVisible = false;
    state.activeVault = id;
    state.activeGroup = null;
    state.activeNav = 'hosts';
    for (const field of ['gatePwd','gatePwdConfirm','gateDavPwd']) $(field).value = '';
    applyVaultData(data,id);
  } catch (error) {
    const label = $('gateError');
    label.textContent = error instanceof Error ? error.message : String(error);
    label.classList.remove('hidden');
  } finally { controls.forEach(el => { el.disabled = false; }); }
}

const vaultIcon = (remote) => remote
  ? '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><rect x="3" y="4" width="18" height="7" rx="2"/><rect x="3" y="13" width="18" height="7" rx="2"/><path d="M7 7.5h1m-1 9h1m7-9h3m-3 9h3"/></svg>'
  : '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><rect x="3" y="4" width="18" height="13" rx="2"/><path d="M8 21h8m-4-4v4"/></svg>';
const folderIcon = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><path d="M3 7a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/></svg>';

function selectVault(id, group = null) {
  state.activeNav = 'hosts';
  state.activeVault = id;
  state.activeGroup = group;
  renderVaultWorkspace();
}

function renderSidebarGroups() {
  const active = (id, group = null) => state.activeNav === 'hosts' && state.activeVault === id && state.activeGroup === group;
  $('navHostCount').textContent = unlockedVaults().reduce((n,v) => n + v.data.hosts.length, 0);
  const branch = (vault) => {
    const id = escapeHtml(vault.id);
    const name = escapeHtml(vault.config ? vault.name : '本機保管庫');
    const expanded = Boolean(vault.data) && !state.collapsedVaults.has(vault.id);
    const groups = [...sortGroups(vault), { id:UNGROUPED, name:'未分組' }];
    const children = groups.map(group => {
      const total = vault.data?.hosts.filter(h => (h.group_id || UNGROUPED) === group.id).length || 0;
      return `<button class="nav-item${active(vault.id,group.id) ? ' active' : ''}" data-vault="${id}" data-group="${escapeHtml(group.id)}"><span class="nav-left">${folderIcon}<span>${escapeHtml(group.name)}</span></span><span class="nav-count">${total}</span></button>`;
    }).join('');
    return `<div class="vault-tree-branch"><button class="nav-item${active(vault.id) ? ' active' : ''}" data-vault="${id}"><span class="nav-left">${vaultIcon(vault.config)}<span title="${name}">${name}</span></span><span class="nav-count">${vault.data ? vault.data.hosts.length : '鎖定'}</span></button>${vault.data ? `<button type="button" class="vault-tree-toggle" data-collapse-vault="${id}" aria-expanded="${expanded}" aria-label="${expanded ? '收合' : '展開'} ${name}"><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="m7 10 5 5 5-5"/></svg></button>` : ''}</div>${expanded ? `<div class="vault-tree-children">${children}</div>` : ''}`;
  };
  const local = getVault(LOCAL_VAULT);
  const remotes = [...state.vaults.values()].filter(v => v.config);
  $('groupNavList').innerHTML = `${local ? `<h2 class="vault-tree-heading">本機</h2>${branch(local)}` : ''}<h2 class="vault-tree-heading">遠端</h2>${remotes.map(branch).join('')}`;
}

function renderHostCard(host, vaultId) {
  const address = host.port && host.port !== 3389 ? `${host.address}:${host.port}` : host.address;
  const username = host.username ? (host.domain ? `${host.domain}\\${host.username}` : host.username) : '—';
  const share = host.shared_folder ? '<span class="tag accent">共享</span>' : '';
  return `<article class="host-card" data-host-card="${escapeHtml(host.id)}" data-owner="${escapeHtml(vaultId)}">
    <div class="host-top"><div><div class="host-name">${vaultIcon(false)} ${escapeHtml(host.name)}</div><div class="host-addr">${escapeHtml(address)}</div></div><div class="host-tags">${share}<span class="tag">${escapeHtml(resolutionLabel(host.width,host.height))}</span></div></div>
    <div class="host-meta"><span class="host-user">${escapeHtml(username)}</span><div class="host-actions"><button class="btn btn-sm" data-host-action="edit">編輯</button><button class="btn btn-sm btn-danger" data-host-action="delete">刪除</button><button class="btn btn-sm btn-primary" data-host-action="connect">連線</button></div></div></article>`;
}

function renderHosts() {
  const vault = getVault(state.activeVault);
  const open = Boolean(vault?.data);
  if (open && state.activeGroup && state.activeGroup !== UNGROUPED && !vault.data.groups.some(g => g.id === state.activeGroup)) state.activeGroup = null;
  const group = vault?.data?.groups.find(g => g.id === state.activeGroup);
  const groupName = group?.name || (state.activeGroup === UNGROUPED ? '未分組' : null);
  const list = vault ? [vault] : unlockedVaults();
  const hostsFor = (v) => (v.data?.hosts || []).filter(h => !state.activeGroup || (h.group_id || UNGROUPED) === state.activeGroup);
  const count = list.reduce((n,v) => n + hostsFor(v).length,0);
  $('hostsTitle').textContent = groupName || vault?.name || '全部主機';
  $('hostsCaption').textContent = vault && !open ? '保管庫已鎖定' : `${groupName ? `${vault.name} / ${groupName} · ` : ''}${count} 台主機${vault ? '' : ` · ${list.length} 個已解鎖保管庫`}`;
  for (const [id,show] of Object.entries({ btnNewGroup:open, btnRenameGroup:!!group, btnDeleteGroup:!!group,
    btnNewHost:!vault || open, btnUnlockVault:!!vault && !open, btnLockVault:open, btnSidebarSync:open && !!vault.config })) {
    $(id).classList.toggle('hidden', !show);
  }
  $('btnSidebarSync').disabled = Boolean(vault?.syncRunning);
  $('hostEmpty').classList.toggle('hidden', !vault || open);
  $('hostEmpty').textContent = '解鎖後即可查看此保管庫';
  $('hostGrid').innerHTML = list.filter(v => v.data).map(v => {
    const hosts = hostsFor(v);
    const groups = [...sortGroups(v), {id:UNGROUPED,name:'未分組'}].filter(g => !state.activeGroup || g.id === state.activeGroup);
    return `<section class="vault-lane"><header><h3>${vaultIcon(v.config)} ${escapeHtml(v.name)}</h3><span class="nav-count">${hosts.length}</span></header>${groups.map(g => {
      const cards = hosts.filter(h => (h.group_id || UNGROUPED) === g.id).map(h => renderHostCard(h,v.id)).join('');
      return cards ? `<h4>${escapeHtml(g.name)}</h4>${cards}` : '';
    }).join('') || '<p class="vault-empty">尚無主機</p>'}</section>`;
  }).join('');
}

function renderVaultWorkspace() {
  renderVaultGate();
  if (appGate.locked || !unlockedVaults().length) return;
  updateNavPanels();
  renderHosts();
  renderSidebarGroups();
  renderVaultSettings();
}

function renderVaultSettings() {
  const owner = settingsOwner();
  if (!owner) return;
  const picker = $('settingsVault');
  picker.replaceChildren(...unlockedVaults().map(v => new Option(v.name,v.id)));
  picker.value = owner.id;
  $('settingsVaultPath').textContent = owner.status.vault_path;
  $('toggleRememberKeychain').classList.toggle('on', Boolean(owner.data.remember_keychain));
  $('toggleMacCmd').classList.toggle('on', owner.data.settings.mac_cmd_to_ctrl);
  $('toggleSelfSigned').classList.toggle('on', owner.data.settings.accept_self_signed_tls);
  $('btnImportVault').hidden = owner.id !== LOCAL_VAULT;
  $('webdavList').innerHTML = [...state.vaults.values()].filter(v => v.config).map(v => {
    const cfg = v.config;
    const address = `${cfg.webdav_url.replace(/\/+$/,'')}/${cfg.webdav_file_name}`;
    const status = v.syncRunning ? '同步中…' : v.syncError || (cfg.last_sync_status && v.data ? cfg.last_sync_status : v.data ? '已解鎖' : '已鎖定');
    return `<div class="webdav-row" data-profile="${escapeHtml(v.id)}"><div><strong>${escapeHtml(v.name)}</strong><div class="vault-caption vault-mono">${escapeHtml(address)}</div><div class="vault-caption" role="status">${escapeHtml(v.testResult || status)}</div></div><div class="vault-actions">
      <button class="btn btn-sm" data-profile-action="test"${v.testRunning?' disabled':''}>測試連線</button><button class="btn btn-sm" data-profile-action="edit">編輯</button>
      ${v.data ? `<button class="btn btn-sm" data-profile-action="sync"${v.syncRunning?' disabled':''}>立即同步</button>` : ''}
      <button class="btn btn-sm" data-profile-action="${v.data?'lock':'unlock'}">${v.data?'鎖定':'解鎖'}</button></div></div>`;
  }).join('') || '<p class="vault-caption">尚未新增 WebDAV 保管庫</p>';
}

function openWebDavEditor(vault) {
  const cfg = vault.config;
  const dialog = $('webdavDialog');
  dialog.dataset.vault = vault.id;
  $('editDavName').value = vault.name;
  $('editDavTarget').textContent = `${cfg.webdav_url}/${cfg.webdav_file_name}`;
  $('editDavUser').value = cfg.webdav_username;
  $('editDavPwd').value = '';
  $('editDavPwd').placeholder = cfg.has_webdav_password ? '已儲存，留空維持原密碼' : '選填';
  $('editDavForget').checked = false;
  $('editDavInsecure').checked = cfg.webdav_allow_invalid_certs;
  $('editDavFeedback').textContent = '';
  dialog.showModal();
  setModalOpenFlag(true);
  $('editDavName').focus();
}

function editConnectionInput(vault) {
  return { mode:'webdav', webdav_url:vault.config.webdav_url, webdav_file_name:vault.config.webdav_file_name,
    webdav_username:$('editDavUser').value.trim(), webdav_password:$('editDavPwd').value || null,
    clear_webdav_password:$('editDavForget').checked, webdav_allow_invalid_certs:$('editDavInsecure').checked };
}

async function testConnection(button, label, args) {
  if (button.disabled) return;
  button.disabled = true;
  label.textContent = '測試連線中…';
  try { label.textContent = await invoke('sync_test_connection',args); }
  catch (e) { label.textContent = String(e); }
  finally { button.disabled = false; }
}

async function lockVault(id) {
  const vault = getVault(id);
  if (!vault?.data) return;
  try {
    vault.status = await invoke('vault_lock',vaultArgs(id));
    delete vault.data;
    state.activeGroup = null;
    if (!unlockedVaults().length) prepareVaultGate(id);
    selectTab('home');
    renderVaultWorkspace();
  } catch (e) { notify(String(e),true); }
}

function updateHostGroupOptions(vaultId, selected) {
  const groups = [...sortGroups(getVault(vaultId)), {id:'',name:'未分組'}];
  $('hostGroup').replaceChildren(...groups.map(g => new Option(g.name,g.id)));
  $('hostGroup').value = groups.some(g => g.id === selected) ? selected : '';
}

async function updateVaultSetting(property) {
  const owner = settingsOwner();
  if (!owner) return;
  try {
    const settings = { ...owner.data.settings, [property]:!owner.data.settings[property] };
    applyVaultData(await invoke('vault_update_settings',{...vaultArgs(owner.id),settings}),owner.id);
  } catch (e) { notify(String(e),true); }
}

async function runSyncNow(id,{silent=false}={}) {
  const vault = getVault(id);
  if (appGate.locked || !vault?.config || !vault.data || vault.syncRunning) return;
  vault.syncRunning = true;
  if (!silent) renderVaultWorkspace();
  try {
    const result = await invoke('sync_now',{vaultId:id});
    const changed = result.added || result.updated || result.deleted;
    const refresh = !silent || result.local_changed || vault.syncError || vault.config.last_sync_status !== result.config.last_sync_status;
    vault.config = result.config;
    vault.syncError = '';
    if (vault.data && refresh) applyVaultData(result.vault_data,id);
    if (!appGate.locked && (!silent || changed)) notify(vault.name + ' · ' + result.summary);
  } catch (error) {
    const message = String(error);
    const refresh = !silent || vault.syncError !== message;
    vault.syncError = message;
    if (refresh && vault.data && !appGate.locked) {
      const data = await invoke('vault_get_data',vaultArgs(id)).catch(() => null);
      if (data && vault.data) applyVaultData(data,id);
    }
    if (!silent && !appGate.locked) notify(message,true);
  } finally {
    vault.syncRunning = false;
    if (!silent) renderVaultWorkspace();
  }
}

function wireVaultUi() {
  if (invoke) setInterval(() => {
    if (!appGate.locked) unlockedVaults().filter(v => v.config).forEach(v => runSyncNow(v.id,{silent:true}));
  },10000);
  $('toggleGateRemember').onclick = () => {
    state.gateRemember = !state.gateRemember;
    $('toggleGateRemember').classList.toggle('on',state.gateRemember);
  };
  $('gateMode').onchange = updateGateMode;
  $('gateForm').onsubmit = submitVaultGate;
  $('btnGateBack').onclick = () => { state.gateVisible = false; renderVaultGate(); };
  $('btnGateTest').onclick = async () => {
    $('gateMode').disabled = true;
    await testConnection($('btnGateTest'),$('gateFeedback'),{input:gateConnectionInput()});
    $('gateMode').disabled = false;
  };
  $('btnAddWebdav').onclick = () => openVaultGate('webdav');
  $('btnUnlockVault').onclick = () => openVaultGate(state.activeVault);
  $('btnLockVault').onclick = () => lockVault(state.activeVault);
  $('btnSidebarSync').onclick = () => runSyncNow(state.activeVault);
  $('groupNavList').onclick = (event) => {
    const button = event.target.closest('button');
    if (button?.dataset.vault) selectVault(button.dataset.vault,button.dataset.group || null);
    const id = button?.dataset.collapseVault;
    if (id) {
      if (state.collapsedVaults.has(id)) state.collapsedVaults.delete(id);
      else state.collapsedVaults.add(id);
      renderSidebarGroups();
      $('groupNavList').querySelector(`[data-collapse-vault="${CSS.escape(id)}"]`)?.focus();
    }
  };
  $('hostGrid').onclick = async (event) => {
    const button = event.target.closest('[data-host-action]');
    const card = button?.closest('[data-host-card]');
    if (!card) return;
    const id = card.dataset.owner;
    const host = hostById(id,card.dataset.hostCard);
    if (!host) return;
    const selected = { ...host, vault_id:id };
    if (button.dataset.hostAction === 'connect') initiateHostConnect(selected);
    else if (button.dataset.hostAction === 'edit') openHostModal(selected);
    else {
      try {
        applyVaultData(await invoke('vault_delete_host',{...vaultArgs(id),id:host.id}),id);
        notify('已刪除主機');
      } catch (e) { notify(String(e),true); }
    }
  };
  $('hostGrid').ondblclick = (event) => {
    if (event.target.closest('button')) return;
    const card = event.target.closest('[data-host-card]');
    if (!card) return;
    const host = hostById(card.dataset.owner,card.dataset.hostCard);
    if (host) initiateHostConnect({ ...host,vault_id:card.dataset.owner });
  };
  $('hostVault').onchange = () => updateHostGroupOptions($('hostVault').value,'');
  $('settingsVault').onchange = () => { state.settingsVault = $('settingsVault').value; renderVaultSettings(); };
  $('toggleRememberKeychain').onclick = async () => {
    const owner = settingsOwner();
    if (!owner) return;
    try {
      const data = await invoke('vault_set_remember_keychain',{...vaultArgs(owner.id),enabled:!owner.data.remember_keychain});
      applyVaultData(data,owner.id);
    } catch (e) { notify(String(e),true); }
  };
  $('toggleMacCmd').onclick = () => updateVaultSetting('mac_cmd_to_ctrl');
  $('toggleSelfSigned').onclick = () => updateVaultSetting('accept_self_signed_tls');
  $('webdavList').onclick = async (event) => {
    const button = event.target.closest('[data-profile-action]');
    const vault = getVault(button?.closest('[data-profile]')?.dataset.profile);
    if (!vault) return;
    const action = button.dataset.profileAction;
    if (action === 'unlock') openVaultGate(vault.id);
    if (action === 'lock') lockVault(vault.id);
    if (action === 'sync') runSyncNow(vault.id);
    if (action === 'edit') openWebDavEditor(vault);
    if (action === 'test' && !vault.testRunning) {
      vault.testRunning = true;
      vault.testResult = '測試連線中…';
      renderVaultSettings();
      try { vault.testResult = await invoke('sync_test_connection',{vaultId:vault.id}); }
      catch (e) { vault.testResult = String(e); }
      vault.testRunning = false;
      renderVaultSettings();
    }
  };
  const dialog = $('webdavDialog');
  dialog.onclose = () => { $('editDavPwd').value = ''; setModalOpenFlag(false); };
  dialog.oncancel = e => { if ($('btnEditDavSave').disabled || $('btnEditDavTest').disabled) e.preventDefault(); };
  $('btnEditDavCancel').onclick = () => dialog.close();
  $('btnEditDavTest').onclick = async () => {
    const vault = getVault(dialog.dataset.vault);
    $('btnEditDavCancel').disabled = true;
    await testConnection($('btnEditDavTest'),$('editDavFeedback'),{vaultId:vault.id,input:editConnectionInput(vault)});
    $('btnEditDavCancel').disabled = false;
  };
  $('webdavEditForm').onsubmit = async (event) => {
    event.preventDefault();
    const controls = ['btnEditDavSave','btnEditDavCancel','btnEditDavTest'].map($);
    if (controls.some(el => el.disabled)) return;
    controls.forEach(el => { el.disabled = true; });
    try {
      const vault = getVault(dialog.dataset.vault);
      const profile = await invoke('sync_save_config',{vaultId:vault.id,name:$('editDavName').value,input:editConnectionInput(vault)});
      saveProfileView(profile);
      dialog.close();
      renderVaultWorkspace();
    } catch (e) { $('editDavFeedback').textContent = String(e); }
    finally { controls.forEach(el => { el.disabled = false; }); }
  };
}
