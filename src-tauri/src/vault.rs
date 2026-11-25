use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Context, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const VAULT_FILE_NAME: &str = "mist-rdp.vault.json";
const BACKUP_DIR_NAME: &str = ".mist-rdp-backups";
const MAX_ROLLING_BACKUPS: usize = 5;
const TOMBSTONE_TTL_MS: u64 = 90 * 24 * 60 * 60 * 1000;
const KEYCHAIN_SERVICE: &str = "pw.shxj.mistrdp";
const KEYCHAIN_ACCOUNT: &str = "vault-master-password";

pub fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn keychain_save(password: &str) -> Result<()> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
        .map_err(|e| anyhow!("鑰匙圈初始化失敗：{e}"))?;
    entry
        .set_password(password)
        .map_err(|e| anyhow!("寫入系統鑰匙圈失敗：{e}"))?;
    Ok(())
}

pub fn keychain_load() -> Option<Zeroizing<String>> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT).ok()?;
    entry.get_password().ok().map(Zeroizing::new)
}

pub fn keychain_delete() -> Result<()> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
        .map_err(|e| anyhow!("鑰匙圈初始化失敗：{e}"))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(anyhow!("移除系統鑰匙圈項目失敗：{e}")),
    }
}

/// Legacy separate account record (kept only for automatic migration of older vault files).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacyVaultAccount {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default = "default_true")]
    pub save_password: bool,
    #[serde(default)]
    pub password: Option<String>,
}

/// Host + paired credentials in a single 1:1 record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedHost {
    pub id: String,
    pub name: String,
    pub address: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_group")]
    pub group: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default = "default_true")]
    pub save_password: bool,
    #[serde(default)]
    pub password: Option<String>,
    /// Legacy reference used only during migration from older vaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// 0 = auto fit window
    #[serde(default)]
    pub width: u16,
    /// 0 = auto fit window
    #[serde(default)]
    pub height: u16,
    #[serde(default = "default_true")]
    pub clipboard_enabled: bool,
    #[serde(default = "default_true")]
    pub keyboard_hook_enabled: bool,
    #[serde(default = "default_true")]
    pub audio_enabled: bool,
    #[serde(default)]
    pub shared_folder: String,
    /// UTC epoch milliseconds of last modification (used for zero-knowledge LWW sync merge).
    #[serde(default)]
    pub updated_at: u64,
    /// Soft-delete tombstone timestamp (UTC epoch ms). Tombstones always strip password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<u64>,
}

impl SavedHost {
    pub fn effective_timestamp(&self) -> u64 {
        self.updated_at.max(self.deleted_at.unwrap_or(0))
    }

    pub fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }
}

/// Public view sent to WebView — never exposes plaintext password.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedHostView {
    pub id: String,
    pub name: String,
    pub address: String,
    pub port: u16,
    pub group: String,
    pub username: String,
    pub domain: String,
    pub save_password: bool,
    pub has_password: bool,
    pub width: u16,
    pub height: u16,
    pub clipboard_enabled: bool,
    pub keyboard_hook_enabled: bool,
    pub audio_enabled: bool,
    pub shared_folder: String,
}

impl From<&SavedHost> for SavedHostView {
    fn from(h: &SavedHost) -> Self {
        Self {
            id: h.id.clone(),
            name: h.name.clone(),
            address: h.address.clone(),
            port: if h.port == 0 { 3389 } else { h.port },
            group: h.group.clone(),
            username: h.username.clone(),
            domain: h.domain.clone(),
            save_password: h.save_password,
            has_password: h.save_password && h.password.as_ref().is_some_and(|p| !p.is_empty()),
            width: h.width,
            height: h.height,
            clipboard_enabled: h.clipboard_enabled,
            keyboard_hook_enabled: h.keyboard_hook_enabled,
            audio_enabled: h.audio_enabled,
            shared_folder: h.shared_folder.clone(),
        }
    }
}

fn default_port() -> u16 {
    3389
}
fn default_group() -> String {
    "預設".to_string()
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppSettings {
    #[serde(default = "default_true")]
    pub mac_cmd_to_ctrl: bool,
    #[serde(default = "default_true")]
    pub accept_self_signed_tls: bool,
    #[serde(default = "default_true")]
    pub lossless_quality: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            mac_cmd_to_ctrl: true,
            accept_self_signed_tls: true,
            lossless_quality: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VaultPayload {
    #[serde(default)]
    pub accounts: Vec<LegacyVaultAccount>,
    #[serde(default)]
    pub hosts: Vec<SavedHost>,
    #[serde(default)]
    pub settings: AppSettings,
    #[serde(default)]
    pub settings_updated_at: u64,
    #[serde(default)]
    pub device_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultPublicData {
    pub hosts: Vec<SavedHostView>,
    pub settings: AppSettings,
    pub vault_path: String,
    pub remember_keychain: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultStatus {
    pub exists: bool,
    pub unlocked: bool,
    pub vault_path: String,
    pub remember_keychain: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEnvelope {
    pub version: u32,
    pub kdf: String,
    pub salt_b64: String,
    pub nonce_b64: String,
    pub ciphertext_b64: String,
    #[serde(default)]
    pub updated_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MergeStats {
    pub added: usize,
    pub updated: usize,
    pub deleted: usize,
    pub local_changed: bool,
    pub remote_needs_update: bool,
}

pub struct VaultManager {
    path: PathBuf,
    key: Option<[u8; 32]>,
    salt: Option<[u8; 16]>,
    master_password: Option<Zeroizing<String>>,
    remember_keychain: bool,
    payload: Option<VaultPayload>,
}

impl Drop for VaultManager {
    fn drop(&mut self) {
        self.lock();
    }
}

fn migrate_legacy_accounts(payload: &mut VaultPayload) -> bool {
    let mut changed = false;
    if !payload.accounts.is_empty() {
        for host in &mut payload.hosts {
            if let Some(acc_id) = host.account_id.take() {
                if host.username.is_empty() {
                    if let Some(acc) = payload.accounts.iter().find(|a| a.id == acc_id) {
                        host.username = acc.username.clone();
                        host.domain = acc.domain.clone();
                        host.save_password = acc.save_password;
                        host.password = if acc.save_password {
                            acc.password.clone()
                        } else {
                            None
                        };
                    }
                }
                changed = true;
            }
        }
        if changed {
            payload.accounts.clear();
        }
    }
    if payload.device_id.is_empty() {
        payload.device_id = Uuid::new_v4().to_string();
        changed = true;
    }
    changed
}

/// Decrypt an encrypted vault envelope JSON string using the given master password.
/// Works seamlessly across devices even when remote vaults were initialized with a different salt.
pub fn decrypt_envelope_with_password(
    json_str: &str,
    master_password: &str,
) -> Result<(VaultPayload, [u8; 32], [u8; 16])> {
    let env: VaultEnvelope = serde_json::from_str(json_str).context("保管庫格式錯誤")?;
    if env.kdf != "argon2id" {
        return Err(anyhow!("不支援的金鑰衍生演算法：{}", env.kdf));
    }
    let salt_vec = B64.decode(&env.salt_b64).context("salt 解碼失敗")?;
    let salt: [u8; 16] = salt_vec
        .try_into()
        .map_err(|_| anyhow!("保管庫 salt 長度錯誤"))?;
    let nonce_vec = B64.decode(&env.nonce_b64).context("nonce 解碼失敗")?;
    if nonce_vec.len() != 12 {
        return Err(anyhow!("保管庫 nonce 長度錯誤"));
    }
    let ciphertext = B64
        .decode(&env.ciphertext_b64)
        .context("ciphertext 解碼失敗")?;

    let key = derive_key(master_password, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| anyhow!("AES 初始化失敗"))?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(Nonce::from_slice(&nonce_vec), ciphertext.as_ref())
            .map_err(|_| anyhow!("主密碼錯誤"))?,
    );

    let mut payload: VaultPayload =
        serde_json::from_slice(&plaintext).context("保管庫內容解析失敗")?;
    migrate_legacy_accounts(&mut payload);
    Ok((payload, key, salt))
}

/// Encrypt a vault payload into a pretty-printed `VaultEnvelope` JSON string with a fresh 96-bit nonce.
pub fn encrypt_payload_to_json(
    key: &[u8; 32],
    salt: &[u8; 16],
    payload: &VaultPayload,
) -> Result<String> {
    let mut json = serde_json::to_vec(payload).context("序列化失敗")?;
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);

    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| anyhow!("AES 初始化失敗"))?;
    let encrypt_res = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), json.as_ref())
        .map_err(|_| anyhow!("加密失敗"));
    json.zeroize();
    let ciphertext = encrypt_res?;

    let env = VaultEnvelope {
        version: 2,
        kdf: "argon2id".to_string(),
        salt_b64: B64.encode(salt),
        nonce_b64: B64.encode(nonce_bytes),
        ciphertext_b64: B64.encode(ciphertext),
        updated_at: now_epoch_ms(),
    };
    serde_json::to_string_pretty(&env).context("信封序列化失敗")
}

/// Deterministic, per-host Last-Write-Wins (LWW) merge with soft-delete tombstones.
pub fn merge_vault_payloads(
    local: &VaultPayload,
    remote: &VaultPayload,
    now_ms: u64,
) -> (VaultPayload, MergeStats) {
    let mut local_norm = local.clone();
    let mut remote_norm = remote.clone();
    migrate_legacy_accounts(&mut local_norm);
    migrate_legacy_accounts(&mut remote_norm);

    let mut stats = MergeStats::default();

    let local_map: HashMap<String, SavedHost> = local_norm
        .hosts
        .iter()
        .cloned()
        .map(|h| (h.id.clone(), h))
        .collect();
    let remote_map: HashMap<String, SavedHost> = remote_norm
        .hosts
        .iter()
        .cloned()
        .map(|h| (h.id.clone(), h))
        .collect();

    let mut ordered_ids: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for h in &local_norm.hosts {
        if seen.insert(h.id.clone()) {
            ordered_ids.push(h.id.clone());
        }
    }
    let mut remote_only_ids: Vec<&SavedHost> = remote_norm
        .hosts
        .iter()
        .filter(|h| !seen.contains(&h.id))
        .collect();
    remote_only_ids.sort_by(|a, b| {
        a.effective_timestamp()
            .cmp(&b.effective_timestamp())
            .then_with(|| a.id.cmp(&b.id))
    });
    for h in remote_only_ids {
        if seen.insert(h.id.clone()) {
            ordered_ids.push(h.id.clone());
        }
    }

    let mut merged_hosts: Vec<SavedHost> = Vec::with_capacity(ordered_ids.len());

    for id in ordered_ids {
        let chosen: SavedHost = match (local_map.get(&id), remote_map.get(&id)) {
            (Some(l), Some(r)) => {
                let l_ts = l.effective_timestamp();
                let r_ts = r.effective_timestamp();
                if r_ts > l_ts {
                    stats.local_changed = true;
                    match (l.is_deleted(), r.is_deleted()) {
                        (false, true) => stats.deleted += 1,
                        (true, false) => stats.added += 1,
                        (false, false) => stats.updated += 1,
                        (true, true) => {}
                    }
                    r.clone()
                } else if l_ts > r_ts {
                    stats.remote_needs_update = true;
                    l.clone()
                } else if r.is_deleted() && !l.is_deleted() {
                    // Exact timestamp tie: deletion wins over live record to avoid resurrection.
                    stats.local_changed = true;
                    stats.deleted += 1;
                    r.clone()
                } else if l.is_deleted() && !r.is_deleted() {
                    stats.remote_needs_update = true;
                    l.clone()
                } else if l != r {
                    // Deterministic lexicographic tie-breaker so both devices converge without ping-ponging.
                    let l_key = (&l.name, &l.address, l.port, &l.username, &l.group);
                    let r_key = (&r.name, &r.address, r.port, &r.username, &r.group);
                    if r_key > l_key {
                        stats.local_changed = true;
                        stats.updated += 1;
                        r.clone()
                    } else {
                        stats.remote_needs_update = true;
                        l.clone()
                    }
                } else {
                    l.clone()
                }
            }
            (None, Some(r)) => {
                stats.local_changed = true;
                if !r.is_deleted() {
                    stats.added += 1;
                }
                r.clone()
            }
            (Some(l), None) => {
                stats.remote_needs_update = true;
                l.clone()
            }
            (None, None) => continue,
        };

        let mut normalized = chosen;
        if normalized.is_deleted() {
            normalized.password = None;
            normalized.save_password = false;
            if let Some(del_ts) = normalized.deleted_at {
                if now_ms > del_ts.saturating_add(TOMBSTONE_TTL_MS) {
                    // Prune expired tombstone older than 90 days.
                    if local_map.contains_key(&id) {
                        stats.local_changed = true;
                    }
                    if remote_map.contains_key(&id) {
                        stats.remote_needs_update = true;
                    }
                    continue;
                }
            }
        }
        merged_hosts.push(normalized);
    }

    let (settings, settings_updated_at) =
        if remote_norm.settings_updated_at > local_norm.settings_updated_at {
            if remote_norm.settings != local_norm.settings {
                stats.local_changed = true;
            }
            (remote_norm.settings.clone(), remote_norm.settings_updated_at)
        } else {
            if local_norm.settings_updated_at > remote_norm.settings_updated_at
                && local_norm.settings != remote_norm.settings
            {
                stats.remote_needs_update = true;
            }
            (local_norm.settings.clone(), local_norm.settings_updated_at)
        };

    // Zeroize temporary host passwords cloned for merging.
    for h in &mut local_norm.hosts {
        if let Some(ref mut pw) = h.password {
            pw.zeroize();
        }
    }
    for h in &mut remote_norm.hosts {
        if let Some(ref mut pw) = h.password {
            pw.zeroize();
        }
    }
    for mut h in local_map.into_values() {
        if let Some(ref mut pw) = h.password {
            pw.zeroize();
        }
    }
    for mut h in remote_map.into_values() {
        if let Some(ref mut pw) = h.password {
            pw.zeroize();
        }
    }

    let merged = VaultPayload {
        accounts: Vec::new(),
        hosts: merged_hosts,
        settings,
        settings_updated_at,
        device_id: if local_norm.device_id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            local_norm.device_id
        },
    };

    (merged, stats)
}

impl VaultManager {
    pub fn new(fallback_dir: PathBuf) -> Self {
        let path = resolve_vault_path(fallback_dir);
        Self {
            path,
            key: None,
            salt: None,
            master_password: None,
            remember_keychain: false,
            payload: None,
        }
    }

    #[cfg(test)]
    pub fn with_exact_path(path: PathBuf) -> Self {
        Self {
            path,
            key: None,
            salt: None,
            master_password: None,
            remember_keychain: false,
            payload: None,
        }
    }

    pub fn status(&self) -> VaultStatus {
        VaultStatus {
            exists: self.path.exists(),
            unlocked: self.payload.is_some(),
            vault_path: self.path.display().to_string(),
            remember_keychain: self.remember_keychain,
        }
    }

    pub fn master_password_clone(&self) -> Result<Zeroizing<String>> {
        self.master_password
            .clone()
            .ok_or_else(|| anyhow!("保管庫尚未解鎖"))
    }

    pub fn create(&mut self, master_password: &str) -> Result<VaultPublicData> {
        if master_password.is_empty() {
            return Err(anyhow!("主密碼不可為空"));
        }
        let mut salt = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut salt);
        let key = derive_key(master_password, &salt)?;
        let payload = VaultPayload {
            device_id: Uuid::new_v4().to_string(),
            ..VaultPayload::default()
        };
        write_encrypted(&self.path, &key, &salt, &payload)?;
        self.key = Some(key);
        self.salt = Some(salt);
        self.master_password = Some(Zeroizing::new(master_password.to_owned()));
        self.payload = Some(payload);
        self.public_data()
    }

    pub fn unlock(&mut self, master_password: &str) -> Result<VaultPublicData> {
        let raw = fs::read_to_string(&self.path)
            .with_context(|| format!("無法讀取保管庫：{}", self.path.display()))?;
        let (payload, key, salt) = decrypt_envelope_with_password(&raw, master_password)?;

        self.key = Some(key);
        self.salt = Some(salt);
        self.master_password = Some(Zeroizing::new(master_password.to_owned()));
        self.payload = Some(payload);
        self.public_data()
    }

    pub fn try_auto_unlock_from_keychain(&mut self) -> Option<VaultPublicData> {
        if !self.path.exists() {
            return None;
        }
        let pwd = keychain_load()?;
        match self.unlock(&pwd) {
            Ok(_) => {
                self.remember_keychain = true;
                self.public_data().ok()
            }
            Err(err) => {
                // Only remove keychain entry when decryption failed due to wrong master password;
                // preserve it on transient file I/O errors.
                if err.to_string().contains("主密碼錯誤") {
                    let _ = keychain_delete();
                    self.remember_keychain = false;
                }
                None
            }
        }
    }

    pub fn set_remember_keychain(&mut self, enable: bool) -> Result<VaultPublicData> {
        if self.payload.is_none() {
            return Err(anyhow!("保管庫尚未解鎖"));
        }
        if enable {
            let pwd = self
                .master_password
                .as_ref()
                .ok_or_else(|| anyhow!("無法取得主密碼"))?;
            keychain_save(pwd)?;
            self.remember_keychain = true;
        } else {
            keychain_delete()?;
            self.remember_keychain = false;
        }
        self.public_data()
    }

    pub fn lock(&mut self) {
        if let Some(mut k) = self.key.take() {
            k.zeroize();
        }
        self.master_password = None;
        self.salt = None;
        if let Some(mut p) = self.payload.take() {
            for h in &mut p.hosts {
                if let Some(ref mut pw) = h.password {
                    pw.zeroize();
                }
            }
            for a in &mut p.accounts {
                if let Some(ref mut pw) = a.password {
                    pw.zeroize();
                }
            }
        }
    }

    pub fn public_data(&self) -> Result<VaultPublicData> {
        let p = self.payload.as_ref().ok_or_else(|| anyhow!("保管庫尚未解鎖"))?;
        Ok(VaultPublicData {
            hosts: p
                .hosts
                .iter()
                .filter(|h| !h.is_deleted())
                .map(SavedHostView::from)
                .collect(),
            settings: p.settings.clone(),
            vault_path: self.path.display().to_string(),
            remember_keychain: self.remember_keychain,
        })
    }

    pub fn save_host(&mut self, mut host: SavedHost) -> Result<VaultPublicData> {
        if !host.save_password {
            host.password = None;
        }
        host.account_id = None;
        if host.port == 0 {
            host.port = 3389;
        }
        host.shared_folder = host.shared_folder.trim().to_string();
        host.deleted_at = None;

        let p = self.payload.as_mut().ok_or_else(|| anyhow!("保管庫尚未解鎖"))?;
        let now = now_epoch_ms();
        if let Some(existing) = p.hosts.iter_mut().find(|h| h.id == host.id) {
            let keep_old = host.save_password
                && host.password.as_deref().unwrap_or("").is_empty()
                && existing.password.is_some();
            if keep_old {
                host.password = existing.password.clone();
            }
            host.updated_at = now.max(existing.effective_timestamp().saturating_add(1));
            *existing = host;
        } else {
            host.updated_at = now;
            p.hosts.push(host);
        }
        self.persist()?;
        self.public_data()
    }

    pub fn update_host_credentials(
        &mut self,
        host_id: &str,
        username: Option<String>,
        password: String,
    ) -> Result<VaultPublicData> {
        let p = self.payload.as_mut().ok_or_else(|| anyhow!("保管庫尚未解鎖"))?;
        let host = p
            .hosts
            .iter_mut()
            .find(|h| h.id == host_id && !h.is_deleted())
            .ok_or_else(|| anyhow!("找不到指定主機"))?;
        if let Some(u) = username.filter(|s| !s.trim().is_empty()) {
            host.username = u.trim().to_string();
        }
        if host.save_password {
            host.password = Some(password);
        }
        host.updated_at = now_epoch_ms().max(host.effective_timestamp().saturating_add(1));
        self.persist()?;
        self.public_data()
    }

    pub fn delete_host(&mut self, id: &str) -> Result<VaultPublicData> {
        let p = self.payload.as_mut().ok_or_else(|| anyhow!("保管庫尚未解鎖"))?;
        let now = now_epoch_ms();
        if let Some(existing) = p.hosts.iter_mut().find(|h| h.id == id) {
            if let Some(ref mut pw) = existing.password {
                pw.zeroize();
            }
            existing.password = None;
            existing.save_password = false;
            let ts = now.max(existing.effective_timestamp().saturating_add(1));
            existing.deleted_at = Some(ts);
            existing.updated_at = ts;
        }
        self.persist()?;
        self.public_data()
    }

    pub fn update_settings(&mut self, settings: AppSettings) -> Result<VaultPublicData> {
        let p = self.payload.as_mut().ok_or_else(|| anyhow!("保管庫尚未解鎖"))?;
        p.settings = settings;
        p.settings_updated_at = now_epoch_ms().max(p.settings_updated_at.saturating_add(1));
        self.persist()?;
        self.public_data()
    }

    pub fn get_host_secret(&self, id: &str) -> Result<SavedHost> {
        let p = self.payload.as_ref().ok_or_else(|| anyhow!("保管庫尚未解鎖"))?;
        p.hosts
            .iter()
            .find(|h| h.id == id && !h.is_deleted())
            .cloned()
            .ok_or_else(|| anyhow!("找不到指定主機"))
    }

    pub fn get_settings(&self) -> AppSettings {
        self.payload
            .as_ref()
            .map(|p| p.settings.clone())
            .unwrap_or_default()
    }

    pub fn export_encrypted_json(&self) -> Result<String> {
        fs::read_to_string(&self.path).context("讀取加密檔案失敗")
    }

    /// Encrypt current in-memory payload into a fresh VaultEnvelope JSON for uploading to a sync target.
    pub fn build_sync_envelope_json(&self) -> Result<String> {
        let key = self.key.as_ref().ok_or_else(|| anyhow!("保管庫尚未解鎖"))?;
        let salt = self.salt.as_ref().ok_or_else(|| anyhow!("salt 遺失"))?;
        let payload = self.payload.as_ref().ok_or_else(|| anyhow!("無資料"))?;
        encrypt_payload_to_json(key, salt, payload)
    }

    /// Merge an already-decrypted remote `VaultPayload` into the current unlocked vault,
    /// persist locally if changed, and return `(MergeStats, merged_envelope_json, VaultPublicData)`.
    pub fn apply_remote_merge(
        &mut self,
        remote: &VaultPayload,
    ) -> Result<(MergeStats, String, VaultPublicData)> {
        let local = self.payload.as_ref().ok_or_else(|| anyhow!("保管庫尚未解鎖"))?;
        let (merged, stats) = merge_vault_payloads(local, remote, now_epoch_ms());
        let key = self.key.as_ref().ok_or_else(|| anyhow!("保管庫尚未解鎖"))?;
        let salt = self.salt.as_ref().ok_or_else(|| anyhow!("salt 遺失"))?;

        if stats.local_changed {
            write_encrypted(&self.path, key, salt, &merged)?;
            self.payload = Some(merged);
        }

        let envelope_json = self.build_sync_envelope_json()?;
        let pub_data = self.public_data()?;
        Ok((stats, envelope_json, pub_data))
    }

    pub fn import_encrypted_json(
        &mut self,
        json_str: &str,
        master_password: &str,
    ) -> Result<VaultPublicData> {
        let (payload, key, salt) = decrypt_envelope_with_password(json_str, master_password)?;

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).ok();
        }
        self.key = Some(key);
        self.salt = Some(salt);
        self.master_password = Some(Zeroizing::new(master_password.to_owned()));
        self.payload = Some(payload);
        self.persist()?;
        if self.remember_keychain {
            let _ = keychain_save(master_password);
        }
        self.public_data()
    }

    fn persist(&self) -> Result<()> {
        let key = self.key.as_ref().ok_or_else(|| anyhow!("保管庫尚未解鎖"))?;
        let salt = self.salt.as_ref().ok_or_else(|| anyhow!("salt 遺失"))?;
        let payload = self.payload.as_ref().ok_or_else(|| anyhow!("無資料"))?;
        write_encrypted(&self.path, key, salt, payload)
    }
}

fn resolve_vault_path(fallback_dir: PathBuf) -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(VAULT_FILE_NAME);
            if candidate.exists() {
                return candidate;
            }
            #[cfg(windows)]
            {
                let probe = dir.join(".mistrdp_write_test");
                if fs::write(&probe, b"").is_ok() {
                    let _ = fs::remove_file(&probe);
                    return candidate;
                }
            }
        }
    }
    let _ = fs::create_dir_all(&fallback_dir);
    fallback_dir.join(VAULT_FILE_NAME)
}

fn derive_key(password: &str, salt: &[u8; 16]) -> Result<[u8; 32]> {
    let params = Params::new(19 * 1024, 2, 1, Some(32))
        .map_err(|e| anyhow!("Argon2 參數錯誤：{e}"))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow!("金鑰衍生失敗：{e}"))?;
    Ok(key)
}

fn create_rolling_backup(path: &Path) {
    if !path.exists() {
        return;
    }
    let Some(parent) = path.parent() else {
        return;
    };
    let backup_dir = parent.join(BACKUP_DIR_NAME);
    if fs::create_dir_all(&backup_dir).is_err() {
        return;
    }
    let backup_name = format!("mist-rdp.{}.{}.vault.json", now_epoch_ms(), Uuid::new_v4().simple());
    let backup_path = backup_dir.join(backup_name);
    if fs::copy(path, &backup_path).is_err() {
        return;
    }
    if let Ok(entries) = fs::read_dir(&backup_dir) {
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|x| x.path()))
            .filter(|p| p.is_file())
            .collect();
        files.sort();
        if files.len() > MAX_ROLLING_BACKUPS {
            let remove_count = files.len() - MAX_ROLLING_BACKUPS;
            for old in files.into_iter().take(remove_count) {
                let _ = fs::remove_file(old);
            }
        }
    }
}

pub fn write_atomic_text(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("vault.json");
    let tmp_path = path.with_file_name(format!(".{}.{}.tmp", file_name, Uuid::new_v4().simple()));
    let write_res = (|| -> Result<()> {
        let mut f = fs::File::create(&tmp_path)
            .with_context(|| format!("建立暫存檔失敗：{}", tmp_path.display()))?;
        f.write_all(content.as_bytes())
            .with_context(|| format!("寫入暫存檔失敗：{}", tmp_path.display()))?;
        f.sync_all().ok();
        Ok(())
    })();
    if let Err(e) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e).with_context(|| format!("更新檔案失敗：{}", path.display()));
    }
    Ok(())
}

fn write_encrypted(
    path: &Path,
    key: &[u8; 32],
    salt: &[u8; 16],
    payload: &VaultPayload,
) -> Result<()> {
    let env_json = encrypt_payload_to_json(key, salt, payload)?;
    create_rolling_backup(path);
    write_atomic_text(path, &env_json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_vault_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("mistrdp_test_{}_{}", name, Uuid::new_v4().simple()));
        fs::create_dir_all(&p).ok();
        p.join("mist-rdp.vault.json")
    }

    fn cleanup_temp_vault(path: &Path) {
        if let Some(dir) = path.parent() {
            let _ = fs::remove_dir_all(dir);
        }
    }

    fn sample_host(id: &str, name: &str, updated_at: u64) -> SavedHost {
        SavedHost {
            id: id.into(),
            name: name.into(),
            address: "10.0.0.1".into(),
            port: 3389,
            group: "預設".into(),
            username: "admin".into(),
            domain: String::new(),
            save_password: true,
            password: Some("Secret123!".into()),
            account_id: None,
            width: 0,
            height: 0,
            clipboard_enabled: true,
            keyboard_hook_enabled: true,
            audio_enabled: true,
            shared_folder: String::new(),
            updated_at,
            deleted_at: None,
        }
    }

    #[test]
    fn vault_paired_host_encrypt_unlock_and_wrong_password() {
        let path = temp_vault_path("paired");
        let mut vm = VaultManager::with_exact_path(path.clone());
        assert!(!vm.status().exists);

        vm.create("SuperSecretMaster!2026").unwrap();
        assert!(vm.status().exists);
        assert!(vm.status().unlocked);

        vm.save_host(SavedHost {
            id: "host-1".into(),
            name: "主力機".into(),
            address: "192.168.1.105".into(),
            port: 3389,
            group: "家中".into(),
            username: "Administrator".into(),
            domain: "CORP".into(),
            save_password: true,
            password: Some("P@ssw0rd_Plaintext_Check".into()),
            account_id: None,
            width: 2560,
            height: 1440,
            clipboard_enabled: true,
            keyboard_hook_enabled: true,
            audio_enabled: true,
            shared_folder: "C:\\Users\\Public\\Share".into(),
            updated_at: 0,
            deleted_at: None,
        })
        .unwrap();

        let on_disk = fs::read_to_string(&path).unwrap();
        assert!(!on_disk.contains("P@ssw0rd_Plaintext_Check"));
        assert!(!on_disk.contains("Administrator"));
        assert!(on_disk.contains("argon2id"));

        vm.lock();
        assert!(!vm.status().unlocked);
        let wrong_err = vm.unlock("WrongPassword").unwrap_err().to_string();
        assert!(wrong_err.contains("主密碼錯誤"));

        let pub_data = vm.unlock("SuperSecretMaster!2026").unwrap();
        assert_eq!(pub_data.hosts.len(), 1);
        assert!(pub_data.hosts[0].has_password);
        assert_eq!(pub_data.hosts[0].width, 2560);
        assert_eq!(pub_data.hosts[0].height, 1440);
        assert!(pub_data.hosts[0].audio_enabled);
        assert_eq!(pub_data.hosts[0].shared_folder, "C:\\Users\\Public\\Share");
        let secret = vm.get_host_secret("host-1").unwrap();
        assert_eq!(secret.password.as_deref(), Some("P@ssw0rd_Plaintext_Check"));

        cleanup_temp_vault(&path);
    }

    #[test]
    fn vault_save_password_false_strips_secret_and_migrates_legacy() {
        let path1 = temp_vault_path("legacy_src");
        let path2 = temp_vault_path("import_dst");

        let mut v1 = VaultManager::with_exact_path(path1.clone());
        v1.create("MasterKey#1").unwrap();

        let pub1 = v1
            .save_host(SavedHost {
                id: "host-ask".into(),
                name: "VPS".into(),
                address: "10.0.0.5".into(),
                port: 3390,
                group: "雲端".into(),
                username: "ops".into(),
                domain: String::new(),
                save_password: false,
                password: Some("ShouldBeStripped".into()),
                account_id: None,
                width: 0,
                height: 0,
                clipboard_enabled: true,
                keyboard_hook_enabled: true,
                audio_enabled: false,
                shared_folder: String::new(),
                updated_at: 0,
                deleted_at: None,
            })
            .unwrap();
        assert!(!pub1.hosts[0].has_password);
        assert!(!pub1.hosts[0].audio_enabled);
        assert_eq!(v1.get_host_secret("host-ask").unwrap().password, None);

        let exported = v1.export_encrypted_json().unwrap();
        let mut v2 = VaultManager::with_exact_path(path2.clone());
        assert!(v2.import_encrypted_json(&exported, "BadKey").is_err());
        let pub2 = v2.import_encrypted_json(&exported, "MasterKey#1").unwrap();
        assert_eq!(pub2.hosts.len(), 1);
        assert_eq!(pub2.hosts[0].username, "ops");
        assert_eq!(pub2.hosts[0].port, 3390);
        assert!(!pub2.hosts[0].has_password);
        assert!(!pub2.hosts[0].audio_enabled);

        cleanup_temp_vault(&path1);
        cleanup_temp_vault(&path2);
    }

    #[test]
    fn merge_lww_handles_concurrent_edits_additions_and_tombstone_deletes() {
        let now = 1_750_000_000_000u64;
        let local = VaultPayload {
            accounts: vec![],
            hosts: vec![
                sample_host("h1", "LocalNewer", now - 100),
                sample_host("h2", "LocalOlder", now - 500),
                sample_host("h3", "OnlyLocal", now - 200),
                SavedHost {
                    deleted_at: Some(now - 50),
                    password: None,
                    save_password: false,
                    ..sample_host("h4", "DeletedLocally", now - 50)
                },
            ],
            settings: AppSettings {
                mac_cmd_to_ctrl: true,
                accept_self_signed_tls: true,
                lossless_quality: true,
            },
            settings_updated_at: now - 500,
            device_id: "dev-a".into(),
        };

        let remote = VaultPayload {
            accounts: vec![],
            hosts: vec![
                sample_host("h1", "RemoteOlder", now - 300),
                sample_host("h2", "RemoteNewer", now - 50),
                sample_host("h4", "StillAliveOnRemote", now - 400),
                sample_host("h5", "OnlyRemote", now - 150),
            ],
            settings: AppSettings {
                mac_cmd_to_ctrl: false,
                accept_self_signed_tls: true,
                lossless_quality: true,
            },
            settings_updated_at: now - 10,
            device_id: "dev-b".into(),
        };

        let (merged, stats) = merge_vault_payloads(&local, &remote, now);
        assert_eq!(stats.added, 1); // h5
        assert_eq!(stats.updated, 1); // h2
        assert_eq!(stats.deleted, 0); // h4 was already deleted locally
        assert!(stats.local_changed);
        assert!(stats.remote_needs_update);

        let by_id: HashMap<&str, &SavedHost> =
            merged.hosts.iter().map(|h| (h.id.as_str(), h)).collect();
        assert_eq!(by_id["h1"].name, "LocalNewer");
        assert_eq!(by_id["h2"].name, "RemoteNewer");
        assert_eq!(by_id["h3"].name, "OnlyLocal");
        assert!(by_id["h4"].is_deleted());
        assert_eq!(by_id["h4"].password, None);
        assert_eq!(by_id["h5"].name, "OnlyRemote");
        assert!(!merged.settings.mac_cmd_to_ctrl);

        // Now verify Device B merging this result also receives the deletion of h4
        let (b_merged, b_stats) = merge_vault_payloads(&remote, &merged, now);
        assert_eq!(b_stats.deleted, 1);
        assert_eq!(b_stats.added, 1); // h3
        assert_eq!(b_stats.updated, 1); // h1
        let b_h4 = b_merged.hosts.iter().find(|h| h.id == "h4").unwrap();
        assert!(b_h4.is_deleted());
        assert_eq!(b_h4.password, None);
    }

    #[test]
    fn delete_host_creates_secretless_tombstone_and_rolling_backups_cap_at_5() {
        let path = temp_vault_path("tombstone_backup");
        let mut vm = VaultManager::with_exact_path(path.clone());
        vm.create("SyncPass#2026").unwrap();

        for i in 0..8 {
            vm.save_host(sample_host(&format!("h-{i}"), &format!("Host {i}"), 0))
                .unwrap();
        }
        let pub_after_del = vm.delete_host("h-0").unwrap();
        assert_eq!(pub_after_del.hosts.len(), 7);
        assert!(vm.get_host_secret("h-0").is_err());

        // Verify backups exist and are capped at MAX_ROLLING_BACKUPS
        let backup_dir = path.parent().unwrap().join(BACKUP_DIR_NAME);
        let count = fs::read_dir(&backup_dir).unwrap().count();
        assert!(count > 0 && count <= MAX_ROLLING_BACKUPS);

        // Verify different-salt decrypt still works
        let exported = vm.build_sync_envelope_json().unwrap();
        let (decrypted, _, _) = decrypt_envelope_with_password(&exported, "SyncPass#2026").unwrap();
        let tomb = decrypted.hosts.iter().find(|h| h.id == "h-0").unwrap();
        assert!(tomb.is_deleted());
        assert_eq!(tomb.password, None);

        cleanup_temp_vault(&path);
    }
}
