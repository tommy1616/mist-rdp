#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app_security;
mod dib;
mod keyboard_hook;
#[cfg(target_os = "macos")]
mod mac_clipboard;
mod profiles;
mod rdp;
mod sync;
mod vault;
mod ws_stream;

use crate::keyboard_hook::{start_hook, HookAction, KeyboardHookState};
use crate::profiles::{PendingProfile, WebDavProfileView, WebDavProfiles};
use crate::rdp::{ConnectParams, RdpManager};
use crate::sync::{
    build_sync_outcome, create_webdav_vault, fetch_webdav_vault, run_sync_with_vault, test_sync_connection,
    SaveSyncConfigInput, SyncMode, SyncOutcome,
};
use crate::vault::{AppSettings, SavedHost, VaultManager, VaultPublicData, VaultStatus};
use crate::ws_stream::StreamHub;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, State, Window, WindowEvent};
use uuid::Uuid;

struct AppState {
    security: Mutex<app_security::AppSecurity>,
    vault: Mutex<VaultManager>,
    profiles: WebDavProfiles,
    rdp: RdpManager,
    kb_hook: Arc<KeyboardHookState>,
    ws_port: u16,
    ws_token: String,
}

#[tauri::command]
fn app_security_status(state: State<'_, AppState>) -> Result<app_security::AppSecurityStatus, String> {
    Ok(state.security.lock().map_err(|e| e.to_string())?.status())
}

#[tauri::command]
fn app_security_password(state: State<'_, AppState>, current: String, next: String) -> Result<app_security::AppSecurityStatus, String> {
    let current = zeroize::Zeroizing::new(current);
    let next = zeroize::Zeroizing::new(next);
    state.security.lock().map_err(|e| e.to_string())?.change_password(&current, &next).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn app_security_idle(state: State<'_, AppState>, minutes: u16) -> Result<app_security::AppSecurityStatus, String> {
    state.security.lock().map_err(|e| e.to_string())?.set_idle(minutes).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn app_lock(state: State<'_, AppState>) -> Result<app_security::AppSecurityStatus, String> {
    state.security.lock().map_err(|e| e.to_string())?.lock().map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn app_unlock(state: State<'_, AppState>, password: String) -> Result<app_security::AppSecurityStatus, String> {
    let password = zeroize::Zeroizing::new(password);
    state.security.lock().map_err(|e| e.to_string())?.unlock(&password).map_err(|e| format!("{e:#}"))
}

#[derive(Serialize)]
struct WebDavLoginResult {
    vault_data: VaultPublicData,
    profile: WebDavProfileView,
}

#[derive(Serialize)]
struct BootstrapInfo {
    ws_port: u16,
    ws_token: String,
    platform: &'static str,
    vault_status: VaultStatus,
}

#[derive(Serialize)]
struct ShareFolderPreset {
    label: String,
    path: String,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
struct SaveHostInput {
    id: Option<String>,
    name: String,
    address: String,
    port: Option<u16>,
    group_id: Option<String>,
    username: String,
    #[serde(default)]
    domain: String,
    save_password: bool,
    password: Option<String>,
    width: u16,
    height: u16,
    clipboard_enabled: bool,
    keyboard_hook_enabled: bool,
    #[serde(default = "default_true")]
    audio_enabled: bool,
    #[serde(default)]
    shared_folder: String,
}

#[derive(Deserialize)]
struct RdpConnectRequest {
    session_id: String,
    host_id: Option<String>,
    vault_id: Option<String>,
    host: String,
    port: Option<u16>,
    username: Option<String>,
    domain: Option<String>,
    password: Option<String>,
    width: u16,
    height: u16,
    clipboard_enabled: bool,
    #[serde(default = "default_true")]
    audio_enabled: bool,
    #[serde(default = "default_true")]
    lossless_quality: bool,
    #[serde(default)]
    shared_folder: Option<String>,
}

#[tauri::command]
fn get_bootstrap_info(state: State<'_, AppState>) -> Result<BootstrapInfo, String> {
    let vault_status = state
        .vault
        .lock()
        .map_err(|e| e.to_string())?
        .status();

    #[cfg(windows)]
    let platform = "windows";
    #[cfg(target_os = "macos")]
    let platform = "macos";
    #[cfg(not(any(windows, target_os = "macos")))]
    let platform = "linux";

    Ok(BootstrapInfo {
        ws_port: state.ws_port,
        ws_token: state.ws_token.clone(),
        platform,
        vault_status,
    })
}

#[tauri::command]
fn get_default_share_folders() -> Vec<ShareFolderPreset> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    let mut presets = Vec::new();
    if let Some(h) = home {
        let candidates = [
            ("桌面", h.join("Desktop")),
            ("下載", h.join("Downloads")),
            ("文件", h.join("Documents")),
            ("家目錄", h),
        ];
        for (label, path) in candidates {
            if path.is_dir() {
                presets.push(ShareFolderPreset {
                    label: label.to_string(),
                    path: path.display().to_string(),
                });
            }
        }
    }
    presets
}

#[tauri::command]
fn vault_status(state: State<'_, AppState>) -> Result<VaultStatus, String> {
    Ok(state.vault.lock().map_err(|e| e.to_string())?.status())
}

#[tauri::command]
fn vault_try_auto_unlock(state: State<'_, AppState>, vault_id: Option<String>) -> Result<Option<VaultPublicData>, String> {
    let profile = vault_id.as_deref().map(|id| state.profiles.get(id)).transpose()?;
    let vault = profile.as_ref().map_or(&state.vault, |profile| &profile.vault);
    let mut v = vault.lock().map_err(|e| e.to_string())?;
    Ok(v.try_auto_unlock_from_keychain())
}

#[tauri::command]
fn vault_create(
    state: State<'_, AppState>,
    master_password: String,
    remember_keychain: Option<bool>,
) -> Result<VaultPublicData, String> {
    let mut v = state.vault.lock().map_err(|e| e.to_string())?;
    let _ = v.create(&master_password).map_err(|e| format!("{e:#}"))?;
    if remember_keychain.unwrap_or(false) {
        let _ = v.set_remember_keychain(true);
    }
    v.public_data().map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn vault_unlock(
    state: State<'_, AppState>,
    vault_id: Option<String>,
    master_password: String,
    remember_keychain: Option<bool>,
) -> Result<VaultPublicData, String> {
    let profile = vault_id.as_deref().map(|id| state.profiles.get(id)).transpose()?;
    let vault = profile.as_ref().map_or(&state.vault, |profile| &profile.vault);
    let mut v = vault.lock().map_err(|e| e.to_string())?;
    let _ = v.unlock(&master_password).map_err(|e| format!("{e:#}"))?;
    if let Some(remember) = remember_keychain {
        let _ = v.set_remember_keychain(remember);
    }
    v.public_data().map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn vault_set_remember_keychain(
    state: State<'_, AppState>,
    vault_id: Option<String>,
    enabled: bool,
) -> Result<VaultPublicData, String> {
    let profile = vault_id.as_deref().map(|id| state.profiles.get(id)).transpose()?;
    let vault = profile.as_ref().map_or(&state.vault, |profile| &profile.vault);
    vault
        .lock()
        .map_err(|e| e.to_string())?
        .set_remember_keychain(enabled)
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn vault_lock(state: State<'_, AppState>, vault_id: Option<String>) -> Result<VaultStatus, String> {
    let profile = vault_id.as_deref().map(|id| state.profiles.get(id)).transpose()?;
    let _guard = profile.as_ref().map(|p| p.gate.try_lock()).transpose().map_err(|_| "請等待 WebDAV 操作完成")?;
    let vault = profile.as_ref().map_or(&state.vault, |profile| &profile.vault);
    let mut v = vault.lock().map_err(|e| e.to_string())?;
    v.lock();
    Ok(v.status())
}

#[tauri::command]
fn vault_get_data(state: State<'_, AppState>, vault_id: Option<String>) -> Result<VaultPublicData, String> {
    let profile = vault_id.as_deref().map(|id| state.profiles.get(id)).transpose()?;
    let vault = profile.as_ref().map_or(&state.vault, |profile| &profile.vault);
    vault
        .lock()
        .map_err(|e| e.to_string())?
        .public_data()
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn vault_save_host(
    state: State<'_, AppState>,
    vault_id: Option<String>,
    input: SaveHostInput,
) -> Result<VaultPublicData, String> {
    let profile = vault_id.as_deref().map(|id| state.profiles.get(id)).transpose()?;
    let vault = profile.as_ref().map_or(&state.vault, |profile| &profile.vault);
    let address = input.address.trim().to_string();
    let name = if input.name.trim().is_empty() {
        address.clone()
    } else {
        input.name.trim().to_string()
    };
    let port = match input.port {
        Some(p) if p > 0 => p,
        _ => 3389,
    };
    let host = SavedHost {
        id: input
            .id
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        name,
        address,
        port,
        group: String::new(),
        group_id: input.group_id,
        username: input.username.trim().to_string(),
        domain: input.domain.trim().to_string(),
        save_password: input.save_password,
        password: if input.save_password {
            input.password
        } else {
            None
        },
        account_id: None,
        width: input.width,
        height: input.height,
        clipboard_enabled: input.clipboard_enabled,
        keyboard_hook_enabled: input.keyboard_hook_enabled,
        audio_enabled: input.audio_enabled,
        shared_folder: input.shared_folder.trim().to_string(),
        updated_at: 0,
        deleted_at: None,
    };
    vault
        .lock()
        .map_err(|e| e.to_string())?
        .save_host(host)
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn vault_update_host_credentials(
    state: State<'_, AppState>,
    vault_id: Option<String>,
    host_id: String,
    username: Option<String>,
    password: String,
) -> Result<VaultPublicData, String> {
    let profile = vault_id.as_deref().map(|id| state.profiles.get(id)).transpose()?;
    let vault = profile.as_ref().map_or(&state.vault, |profile| &profile.vault);
    vault
        .lock()
        .map_err(|e| e.to_string())?
        .update_host_credentials(&host_id, username, password)
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn vault_delete_host(
    state: State<'_, AppState>,
    vault_id: Option<String>,
    id: String,
) -> Result<VaultPublicData, String> {
    let profile = vault_id.as_deref().map(|id| state.profiles.get(id)).transpose()?;
    let vault = profile.as_ref().map_or(&state.vault, |profile| &profile.vault);
    vault
        .lock()
        .map_err(|e| e.to_string())?
        .delete_host(&id)
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn vault_change_group(
    state: State<'_, AppState>,
    vault_id: Option<String>,
    id: Option<String>,
    name: Option<String>,
) -> Result<VaultPublicData, String> {
    let profile = vault_id.as_deref().map(|id| state.profiles.get(id)).transpose()?;
    let vault = profile.as_ref().map_or(&state.vault, |profile| &profile.vault);
    vault.lock().map_err(|e| e.to_string())?
        .change_group(id.as_deref(), name.as_deref())
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn vault_update_settings(
    state: State<'_, AppState>,
    vault_id: Option<String>,
    settings: AppSettings,
) -> Result<VaultPublicData, String> {
    let profile = vault_id.as_deref().map(|id| state.profiles.get(id)).transpose()?;
    let vault = profile.as_ref().map_or(&state.vault, |profile| &profile.vault);
    vault
        .lock()
        .map_err(|e| e.to_string())?
        .update_settings(settings)
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn vault_export_json(state: State<'_, AppState>, vault_id: Option<String>) -> Result<String, String> {
    let profile = vault_id.as_deref().map(|id| state.profiles.get(id)).transpose()?;
    let vault = profile.as_ref().map_or(&state.vault, |profile| &profile.vault);
    vault
        .lock()
        .map_err(|e| e.to_string())?
        .export_encrypted_json()
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn webdav_profiles(state: State<'_, AppState>) -> Result<Vec<WebDavProfileView>, String> {
    state.profiles.list()
}

fn profile_name(raw: &str) -> Result<&str, String> {
    let name = raw.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err("請輸入 1 到 60 字的保管庫名稱".into());
    }
    Ok(name)
}

#[tauri::command]
fn sync_save_config(
    state: State<'_, AppState>,
    vault_id: String,
    name: String,
    mut input: SaveSyncConfigInput,
) -> Result<WebDavProfileView, String> {
    let profile = state.profiles.get(&vault_id)?;
    let _guard = profile.gate.try_lock().map_err(|_| "請等待 WebDAV 操作完成")?;
    let name = profile_name(&name)?;
    input.mode = SyncMode::WebDav;
    {
        let mut sync = profile.sync.lock().map_err(|e| e.to_string())?;
        let configured = sync.config_clone().target_url().map_err(|e| format!("{e:#}"))?;
        let requested = input.connection_config().and_then(|cfg| cfg.target_url()).map_err(|e| format!("{e:#}"))?;
        if requested != configured {
            return Err("要使用不同的網址或檔名，請新增另一個 WebDAV 保管庫".into());
        }
        sync.save_config(input).map_err(|e| format!("{e:#}"))?;
    }
    profile.set_name(name).map_err(|e| format!("{e:#}"))?;
    profile.view()
}

#[tauri::command]
async fn sync_test_connection(
    state: State<'_, AppState>,
    vault_id: Option<String>,
    input: Option<SaveSyncConfigInput>,
) -> Result<String, String> {
    let profile = vault_id.as_deref().map(|id| state.profiles.get(id)).transpose()?;
    let (cfg, password) = match input {
        Some(mut input) => {
            input.mode = SyncMode::WebDav;
            let cfg = input.connection_config().map_err(|e| format!("{e:#}"))?;
            let password = if let Some(profile) = &profile {
                profile.sync.lock().map_err(|e| e.to_string())?.connection_password(&cfg, &input)
            } else {
                input.webdav_password.filter(|s| !s.is_empty()).map(zeroize::Zeroizing::new)
            };
            (cfg,password)
        }
        None => {
            let profile = profile.ok_or("請選擇 WebDAV 保管庫")?;
            let sync = profile.sync.lock().map_err(|e| e.to_string())?;
            (sync.config_clone(), sync.stored_password())
        }
    };
    test_sync_connection(&cfg, password).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn vault_webdav_login(
    state: State<'_, AppState>,
    name: String,
    mut input: SaveSyncConfigInput,
    master_password: String,
    remember_keychain: bool,
    create: bool,
) -> Result<WebDavLoginResult, String> {
    let _guard = state.profiles.registration.lock().await;
    let name = profile_name(&name)?;
    input.mode = SyncMode::WebDav;
    let cfg = input.connection_config().map_err(|e| format!("{e:#}"))?;
    let target = cfg.target_url().map_err(|e| format!("{e:#}"))?;
    if state.profiles.has_target(&target)? {
        return Err("這個 WebDAV 保管庫已新增，請選擇該保管庫解鎖".into());
    }
    let pass = zeroize::Zeroizing::new(master_password);
    let password = input.webdav_password.as_ref().filter(|s| !s.is_empty());
    let raw = if create { None } else {
        Some(fetch_webdav_vault(&cfg, password).await.map_err(|e| format!("{e:#}"))?)
    };
    let profile = state.profiles.draft();
    let mut pending = PendingProfile::new(Arc::clone(&profile));
    let encrypted = {
        let mut vault = profile.vault.lock().map_err(|e| e.to_string())?;
        if let Some(raw) = raw {
            vault.restore_webdav(&raw, &pass).map_err(|e| format!("{e:#}"))?;
            None
        } else {
            vault.create(&pass).map_err(|e| format!("{e:#}"))?;
            Some(vault.export_encrypted_json().map_err(|e| format!("{e:#}"))?)
        }
    };
    if let Some(encrypted) = encrypted {
        create_webdav_vault(&cfg, password, encrypted).await.map_err(|e| format!("{e:#}"))?;
    }
    let local_result = (|| -> Result<WebDavLoginResult, String> {
        profile.sync.lock().map_err(|e| e.to_string())?.save_config(input)
            .map_err(|e| format!("{e:#}"))?;
        let vault_data = {
            let mut vault = profile.vault.lock().map_err(|e| e.to_string())?;
            if remember_keychain {
                vault.set_remember_keychain(true).map_err(|e| format!("{e:#}"))?;
            }
            vault.public_data().map_err(|e| format!("{e:#}"))?
        };
        profile.set_name(name).map_err(|e| format!("{e:#}"))?;
        Ok(WebDavLoginResult { vault_data, profile: profile.view()? })
    })();
    let result = local_result.map_err(|e| if create {
        format!("遠端已建立，但本機設定儲存失敗，可用「連線既有保管庫」復原：{e}")
    } else { e })?;
    state.profiles.add(profile)?;
    pending.commit();
    Ok(result)
}

#[tauri::command]
async fn remote_vault_unlock(
    state: State<'_, AppState>,
    vault_id: String,
    master_password: String,
    remember_keychain: bool,
) -> Result<VaultPublicData, String> {
    let profile = state.profiles.get(&vault_id)?;
    let _guard = profile.gate.lock().await;
    let cached = profile.vault.lock().map_err(|e| e.to_string())?.status().exists;
    let raw = if cached { None } else {
        let (cfg, password) = {
            let sync = profile.sync.lock().map_err(|e| e.to_string())?;
            (sync.config_clone(), sync.stored_password())
        };
        Some(fetch_webdav_vault(&cfg, password.as_deref()).await.map_err(|e| format!("{e:#}"))?)
    };
    let pass = zeroize::Zeroizing::new(master_password);
    let mut vault = profile.vault.lock().map_err(|e| e.to_string())?;
    if let Some(raw) = raw {
        vault.restore_webdav(&raw, &pass).map_err(|e| format!("{e:#}"))?;
    } else {
        vault.unlock(&pass).map_err(|e| format!("{e:#}"))?;
    }
    if let Err(error) = vault.set_remember_keychain(remember_keychain) {
        vault.lock();
        return Err(format!("{error:#}"));
    }
    vault.public_data().map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn sync_now(state: State<'_, AppState>, vault_id: String) -> Result<SyncOutcome, String> {
    let profile = state.profiles.get(&vault_id)?;
    let _guard = profile.gate.lock().await;
    let (cfg, password) = {
        let sync = profile.sync.lock().map_err(|e| e.to_string())?;
        (sync.config_clone(), sync.stored_password())
    };
    let (stats, uploaded, pulled, vault_data) = run_sync_with_vault(&profile.vault, &cfg, password)
        .await.map_err(|e| format!("{e:#}"))?;
    Ok(build_sync_outcome(stats, uploaded, pulled, vault_data, &profile.sync))
}

#[tauri::command]
fn vault_import_json(
    state: State<'_, AppState>,
    vault_id: Option<String>,
    json_str: String,
    master_password: String,
) -> Result<VaultPublicData, String> {
    let profile = vault_id.as_deref().map(|id| state.profiles.get(id)).transpose()?;
    let _guard = profile.as_ref().map(|p| p.gate.try_lock()).transpose().map_err(|_| "請等待 WebDAV 操作完成")?;
    let vault = profile.as_ref().map_or(&state.vault, |profile| &profile.vault);
    vault
        .lock()
        .map_err(|e| e.to_string())?
        .import_encrypted_json(&json_str, &master_password)
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn rdp_connect(state: State<'_, AppState>, req: RdpConnectRequest) -> Result<(), String> {
    let (username, domain, password, accept_self_signed_tls, shared_folder) = {
        let profile = req.vault_id.as_deref().map(|id| state.profiles.get(id)).transpose()?;
        let vault = profile.as_ref().map_or(&state.vault, |p| &p.vault).lock().map_err(|e| e.to_string())?;
        let settings = vault.get_settings();

        if let Some(hid) = req.host_id.as_deref().filter(|s| !s.is_empty()) {
            let saved = vault
                .get_host_secret(hid)
                .map_err(|e| format!("{e:#}"))?;
            let user = req
                .username
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| saved.username.clone());
            if user.is_empty() {
                return Err("NEED_CREDENTIALS".to_string());
            }
            let pwd = if let Some(p) = req.password.filter(|s| !s.is_empty()) {
                p
            } else if saved.save_password {
                saved.password.unwrap_or_default()
            } else {
                return Err("NEED_PASSWORD".to_string());
            };
            let dom = req
                .domain
                .filter(|d| !d.trim().is_empty())
                .or_else(|| {
                    if saved.domain.is_empty() {
                        None
                    } else {
                        Some(saved.domain)
                    }
                });
            let folder = req
                .shared_folder
                .filter(|s| !s.trim().is_empty())
                .or_else(|| {
                    if saved.shared_folder.trim().is_empty() {
                        None
                    } else {
                        Some(saved.shared_folder)
                    }
                });
            (user, dom, pwd, settings.accept_self_signed_tls, folder)
        } else {
            let user = req.username.unwrap_or_default().trim().to_string();
            let pwd = req.password.unwrap_or_default();
            if user.is_empty() {
                return Err("請輸入帳號".to_string());
            }
            let folder = req.shared_folder.filter(|s| !s.trim().is_empty());
            (
                user,
                req.domain.filter(|d| !d.trim().is_empty()),
                pwd,
                settings.accept_self_signed_tls,
                folder,
            )
        }
    };

    // Split DOMAIN\user if user typed backslash form
    let (final_user, final_domain) = if domain.is_none() && username.contains('\\') {
        let mut parts = username.splitn(2, '\\');
        let d = parts.next().unwrap_or("").to_string();
        let u = parts.next().unwrap_or("").to_string();
        (u, Some(d))
    } else {
        (username, domain)
    };

    let port = match req.port {
        Some(p) if p > 0 => p,
        _ => 3389,
    };

    state
        .rdp
        .connect(ConnectParams {
            session_id: req.session_id,
            host: req.host,
            port,
            username: final_user,
            domain: final_domain,
            password,
            width: req.width,
            height: req.height,
            clipboard_enabled: req.clipboard_enabled,
            audio_enabled: req.audio_enabled,
            lossless_quality: req.lossless_quality,
            shared_folder,
            accept_self_signed_tls,
        })
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn rdp_disconnect(state: State<'_, AppState>, session_id: String) {
    state.rdp.disconnect(&session_id);
}

#[tauri::command]
fn rdp_set_audio_enabled(state: State<'_, AppState>, session_id: String, enabled: bool) {
    state.rdp.set_audio_enabled(&session_id, enabled);
}

#[tauri::command]
fn rdp_key(
    state: State<'_, AppState>,
    session_id: String,
    scancode: u8,
    extended: bool,
    pressed: bool,
) {
    state.rdp.send_key(&session_id, scancode, extended, pressed);
}

#[tauri::command]
fn rdp_unicode(
    state: State<'_, AppState>,
    session_id: String,
    code_unit: u16,
    pressed: bool,
) {
    state.rdp.send_unicode(&session_id, code_unit, pressed);
}

#[tauri::command]
fn rdp_mouse(
    state: State<'_, AppState>,
    session_id: String,
    x: u16,
    y: u16,
    button: u8,
    action: String,
    wheel_delta: i16,
) {
    state
        .rdp
        .send_mouse(&session_id, x, y, button, &action, wheel_delta);
}

#[tauri::command]
fn rdp_ctrl_alt_del(state: State<'_, AppState>, session_id: String) {
    state.rdp.send_ctrl_alt_del(&session_id);
}

#[tauri::command]
fn rdp_pause(state: State<'_, AppState>, session_id: String) {
    state.rdp.send_pause(&session_id);
}

#[tauri::command]
fn rdp_sync_locks(
    state: State<'_, AppState>,
    session_id: String,
    scroll: bool,
    num: bool,
    caps: bool,
) {
    state.rdp.sync_locks(&session_id, scroll, num, caps);
}

#[tauri::command]
fn rdp_remote_action(state: State<'_, AppState>, session_id: String, action: String) {
    match action.as_str() {
        "screenshot_window" => state.rdp.send_remote_screenshot(&session_id, true),
        "screenshot_screen" => state.rdp.send_remote_screenshot(&session_id, false),
        "start_menu" => state.rdp.send_remote_start_menu(&session_id),
        "window_menu" => state.rdp.send_remote_window_menu(&session_id),
        "cycle_order" => state.rdp.send_remote_cycle_order(&session_id),
        "switch_app_next" => state.rdp.send_remote_switch_app(&session_id, false),
        "switch_app_prev" => state.rdp.send_remote_switch_app(&session_id, true),
        _ => {}
    }
}

#[tauri::command]
fn rdp_release_modifiers(state: State<'_, AppState>, session_id: String) {
    state.rdp.release_all_modifiers(&session_id);
}

#[tauri::command]
fn rdp_resize(state: State<'_, AppState>, session_id: String, width: u16, height: u16) {
    state.rdp.resize(&session_id, width, height);
}

#[tauri::command]
fn set_active_rdp_tab(state: State<'_, AppState>, session_id: Option<String>) {
    state.rdp.set_active_session(session_id.as_deref());
    state.kb_hook.set_active_session(session_id);
}

#[tauri::command]
fn set_canvas_focused(state: State<'_, AppState>, focused: bool) {
    state.kb_hook.set_canvas_focused(focused);
    if !focused {
        if let Some(sid) = state.kb_hook.get_active_session() {
            state.rdp.release_all_modifiers(&sid);
        }
    }
}

#[tauri::command]
fn set_keyboard_hook(state: State<'_, AppState>, enabled: bool) {
    state.kb_hook.set_enabled(enabled);
    if !enabled {
        if let Some(sid) = state.kb_hook.get_active_session() {
            state.rdp.release_all_modifiers(&sid);
        }
    }
}

#[tauri::command]
fn set_modal_open(state: State<'_, AppState>, open: bool) {
    state.kb_hook.set_modal_open(open);
    if open {
        if let Some(sid) = state.kb_hook.get_active_session() {
            state.rdp.release_all_modifiers(&sid);
        }
    }
}

#[tauri::command]
fn win_minimize(window: Window) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        // macOS AppKit ignores miniaturize() while a window is in a native fullscreen Space.
        if window.is_fullscreen().unwrap_or(false) {
            let _ = window.set_fullscreen(false);
            let _ = window.emit("fullscreen-changed", false);
            let win = window.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(550));
                let w = win.clone();
                let _ = win.run_on_main_thread(move || {
                    let _ = w.minimize();
                });
            });
            return Ok(());
        }
    }
    window.minimize().map_err(|e| e.to_string())
}

#[tauri::command]
fn win_toggle_maximize(window: Window) -> Result<bool, String> {
    let maxed = window.is_maximized().map_err(|e| e.to_string())?;
    if maxed {
        window.unmaximize().map_err(|e| e.to_string())?;
        Ok(false)
    } else {
        window.maximize().map_err(|e| e.to_string())?;
        Ok(true)
    }
}

#[tauri::command]
fn win_toggle_fullscreen(window: Window) -> Result<bool, String> {
    let fs = window.is_fullscreen().map_err(|e| e.to_string())?;
    let next = !fs;
    window.set_fullscreen(next).map_err(|e| e.to_string())?;
    Ok(next)
}

#[tauri::command]
fn win_set_fullscreen(window: Window, fullscreen: bool) -> Result<bool, String> {
    window
        .set_fullscreen(fullscreen)
        .map_err(|e| e.to_string())?;
    Ok(fullscreen)
}

#[tauri::command]
fn win_is_fullscreen(window: Window) -> Result<bool, String> {
    window.is_fullscreen().map_err(|e| e.to_string())
}

#[tauri::command]
fn win_close(window: Window) -> Result<(), String> {
    window.close().map_err(|e| e.to_string())
}

#[tauri::command]
fn win_start_drag(window: Window) -> Result<(), String> {
    window.start_dragging().map_err(|e| e.to_string())
}

fn runtime_log_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library/Logs/MistRDP.log"))
            .unwrap_or_else(|| std::env::temp_dir().join("MistRDP.log"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::temp_dir().join("MistRDP.log")
    }
}

#[derive(Clone)]
struct RollingFileMakeWriter {
    file: Arc<Mutex<std::fs::File>>,
}

struct LockedFileWriter<'a> {
    guard: std::sync::MutexGuard<'a, std::fs::File>,
}

impl std::io::Write for LockedFileWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.guard.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.guard.flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RollingFileMakeWriter {
    type Writer = LockedFileWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        let guard = self.file.lock().unwrap_or_else(|e| e.into_inner());
        LockedFileWriter { guard }
    }
}

fn init_tracing_logger() {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(
            "info,ironrdp_session=info,ironrdp_cliprdr=info,ironrdp_cliprdr_native=info",
        )
    });

    let log_path = runtime_log_path();
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
    if std::fs::metadata(&log_path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_path);
    }

    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let writer = RollingFileMakeWriter {
            file: Arc::new(Mutex::new(file)),
        };
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_ansi(false)
            .with_writer(writer)
            .init();
        tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            log = %log_path.display(),
            "MistRDP logger initialized"
        );
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .init();
    }
}

fn install_panic_logger() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        #[cfg(target_os = "macos")]
        let log_path = std::env::var_os("HOME")
            .map(|h| std::path::PathBuf::from(h).join("Library/Logs/MistRDP-panic.log"))
            .unwrap_or_else(|| std::env::temp_dir().join("MistRDP-panic.log"));
        #[cfg(not(target_os = "macos"))]
        let log_path = std::env::temp_dir().join("MistRDP-panic.log");

        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = writeln!(f, "--- MistRDP panic ---\n{info}\n");
        }
        prev(info);
    }));
}

fn main() {
    install_panic_logger();
    let _ = rustls::crypto::ring::default_provider().install_default();

    init_tracing_logger();

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let rt_handle = rt.handle().clone();
    let hub = rt
        .block_on(StreamHub::start())
        .expect("start loopback stream hub");
    let ws_port = hub.port;
    let ws_token = hub.token.clone();

    let rdp = RdpManager::new(hub, rt_handle);
    let kb_hook = KeyboardHookState::new();
    {
        let rdp_for_hook = rdp.clone();
        kb_hook.set_sink(Arc::new(move |sid, ev| {
            rdp_for_hook.send_key(sid, ev.scancode, ev.extended, ev.pressed);
        }));
    }
    start_hook(kb_hook.clone());

    let kb_hook_for_events = kb_hook.clone();
    let rdp_for_events = rdp.clone();

    // Critical on Windows (tauri-apps/wry#1761): unregister Tao's same-process Raw Input so
    // WH_KEYBOARD_LL keeps receiving Alt+Tab / Win / Right Shift while WebView2 has focus.
    tauri::Builder::default()
        .device_event_filter(tauri::DeviceEventFilter::Always)
        .on_window_event(move |window, event| match event {
            WindowEvent::Focused(focused) => {
                kb_hook_for_events.set_focused(*focused);
                if !*focused {
                    if let Some(sid) = kb_hook_for_events.get_active_session() {
                        rdp_for_events.release_all_modifiers(&sid);
                    }
                } else {
                    #[cfg(windows)]
                    if let Some(sid) = kb_hook_for_events.get_active_session() {
                        let (scroll, num, caps) = crate::keyboard_hook::read_local_lock_states();
                        rdp_for_events.sync_locks(&sid, scroll, num, caps);
                    }
                    let _ = window.emit("window-focused", true);
                }
            }
            WindowEvent::Resized(_) => {
                if let Ok(fs) = window.is_fullscreen() {
                    let _ = window.emit("fullscreen-changed", fs);
                }
            }
            _ => {}
        })
        .setup(move |app| {
            let fallback_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("MistRDP"));
            let profiles = WebDavProfiles::new(&fallback_dir)?;
            let security = app_security::AppSecurity::load(fallback_dir.join("app-security.json"))?;
            let vault = VaultManager::new(fallback_dir);

            #[cfg(windows)]
            if let Some(win) = app.get_webview_window("main") {
                if let Ok(hwnd) = win.hwnd() {
                    kb_hook.set_target_hwnd(hwnd.0 as isize);
                }
            }

            {
                let app_handle = app.handle().clone();
                kb_hook.set_on_enabled_change(Arc::new(move |enabled| {
                    let _ = app_handle.emit("hook-state-changed", enabled);
                }));
            }
            {
                let app_handle = app.handle().clone();
                let rdp_for_actions = rdp.clone();
                kb_hook.set_action_sink(Arc::new(move |sid, action| match action {
                    HookAction::SendCtrlAltDel => {
                        rdp_for_actions.send_ctrl_alt_del(sid);
                    }
                    HookAction::SendPause => {
                        rdp_for_actions.send_pause(sid);
                    }
                    HookAction::ToggleFullscreen => {
                        rdp_for_actions.release_all_modifiers(sid);
                        let ah = app_handle.clone();
                        let _ = app_handle.run_on_main_thread(move || {
                            if let Some(win) = ah.get_webview_window("main") {
                                if let Ok(fs) = win.is_fullscreen() {
                                    let next = !fs;
                                    if win.set_fullscreen(next).is_ok() {
                                        let _ = ah.emit("fullscreen-changed", next);
                                    }
                                }
                            }
                        });
                    }
                    HookAction::ShowHud => {
                        let _ = app_handle.emit("show-hud", ());
                    }
                    HookAction::RemoteScreenshot { active_window_only } => {
                        rdp_for_actions.send_remote_screenshot(sid, active_window_only);
                    }
                    HookAction::RemoteStartMenu => {
                        rdp_for_actions.send_remote_start_menu(sid);
                    }
                    HookAction::RemoteWindowMenu => {
                        rdp_for_actions.send_remote_window_menu(sid);
                    }
                    HookAction::RemoteCycleOrder => {
                        rdp_for_actions.send_remote_cycle_order(sid);
                    }
                    HookAction::RemoteSwitchApp { reverse } => {
                        rdp_for_actions.send_remote_switch_app(sid, reverse);
                    }
                }));
            }

            std::mem::forget(rt);

            app.manage(AppState {
                security: Mutex::new(security),
                vault: Mutex::new(vault),
                profiles,
                rdp,
                kb_hook,
                ws_port,
                ws_token,
            });
            Ok(())
        })
        .invoke_handler({
        let dispatch: fn(tauri::ipc::Invoke<tauri::Wry>) -> bool = tauri::generate_handler![
            app_security_status,
            app_security_password,
            app_security_idle,
            app_lock,
            app_unlock,
            get_bootstrap_info,
            get_default_share_folders,
            vault_status,
            vault_try_auto_unlock,
            vault_create,
            vault_unlock,
            vault_set_remember_keychain,
            vault_lock,
            vault_get_data,
            vault_save_host,
            vault_update_host_credentials,
            vault_delete_host,
            vault_change_group,
            vault_webdav_login,
            vault_update_settings,
            vault_export_json,
            vault_import_json,
            webdav_profiles,
            remote_vault_unlock,
            sync_save_config,
            sync_test_connection,
            sync_now,
            rdp_connect,
            rdp_disconnect,
            rdp_set_audio_enabled,
            rdp_key,
            rdp_unicode,
            rdp_mouse,
            rdp_ctrl_alt_del,
            rdp_pause,
            rdp_sync_locks,
            rdp_remote_action,
            rdp_release_modifiers,
            rdp_resize,
            set_active_rdp_tab,
            set_canvas_focused,
            set_keyboard_hook,
            set_modal_open,
            win_minimize,
            win_toggle_maximize,
            win_toggle_fullscreen,
            win_set_fullscreen,
            win_is_fullscreen,
            win_close,
            win_start_drag,
        ];
        move |invoke: tauri::ipc::Invoke<tauri::Wry>| {
            let command = invoke.message.command();
            let allowed_while_locked = matches!(command, "app_security_status" | "app_unlock" |
                "win_minimize" | "win_toggle_maximize" | "win_set_fullscreen" | "win_is_fullscreen" | "win_start_drag" | "win_close");
            if !allowed_while_locked {
                let webview = invoke.message.webview();
                let locked = webview.try_state::<AppState>()
                    .and_then(|state| state.security.lock().ok().map(|security| security.status().locked))
                    .unwrap_or(true);
                if locked {
                    invoke.resolver.reject("App 已鎖定");
                    return true;
                }
            }
            dispatch(invoke)
        }
        })
        .run(tauri::generate_context!())
        .expect("error while running MistRDP");
}
