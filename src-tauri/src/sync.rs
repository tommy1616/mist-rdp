use crate::vault::{
    decrypt_envelope_with_password, now_epoch_ms, write_atomic_text, MergeStats, VaultManager,
    VaultPublicData,
};
use anyhow::{anyhow, Context, Result};
use reqwest::header::{CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use zeroize::Zeroizing;

const SYNC_CONFIG_FILE_NAME: &str = "mist-rdp.sync.json";
const DEFAULT_SYNC_FILE_NAME: &str = "mist-rdp.vault.json";
const KEYCHAIN_SERVICE: &str = "pw.shxj.mistrdp";
const KEYCHAIN_WEBDAV_ACCOUNT: &str = "sync-webdav-password";
const MAX_REMOTE_VAULT_BYTES: usize = 10 * 1024 * 1024; // 10 MiB safety cap
const MAX_WEBDAV_RETRIES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SyncMode {
    #[default]
    #[serde(alias = "folder")]
    Disabled,
    WebDav,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    #[serde(default)]
    pub mode: SyncMode,
    #[serde(default)]
    pub webdav_url: String,
    #[serde(default)]
    pub webdav_username: String,
    #[serde(default = "default_sync_filename")]
    pub webdav_file_name: String,
    #[serde(default)]
    pub webdav_allow_invalid_certs: bool,
    #[serde(default)]
    pub last_sync_at: Option<u64>,
    #[serde(default)]
    pub last_sync_status: String,
}

fn default_sync_filename() -> String {
    DEFAULT_SYNC_FILE_NAME.to_string()
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            mode: SyncMode::Disabled,
            webdav_url: String::new(),
            webdav_username: String::new(),
            webdav_file_name: default_sync_filename(),
            webdav_allow_invalid_certs: false,
            last_sync_at: None,
            last_sync_status: String::new(),
        }
    }
}

impl SyncConfig {
    pub fn target_url(&self) -> Result<String> {
        let url = build_webdav_target_url(&self.webdav_url, &self.webdav_file_name)?;
        Ok(reqwest::Url::parse(&url)?.to_string())
    }
}

/// Public sync config sent to WebView — never exposes WebDAV password.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfigPublic {
    pub mode: SyncMode,
    pub webdav_url: String,
    pub webdav_username: String,
    pub webdav_file_name: String,
    pub webdav_allow_invalid_certs: bool,
    pub has_webdav_password: bool,
    pub last_sync_at: Option<u64>,
    pub last_sync_status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SaveSyncConfigInput {
    pub mode: SyncMode,
    #[serde(default)]
    pub webdav_url: String,
    #[serde(default)]
    pub webdav_username: String,
    #[serde(default)]
    pub webdav_file_name: String,
    #[serde(default)]
    pub webdav_allow_invalid_certs: bool,
    #[serde(default)]
    pub webdav_password: Option<String>,
    #[serde(default)]
    pub clear_webdav_password: bool,
}

impl SaveSyncConfigInput {
    pub fn connection_config(&self) -> Result<SyncConfig> {
        let webdav_url = if self.mode == SyncMode::WebDav {
            validate_webdav_url(&self.webdav_url)?
        } else {
            self.webdav_url.trim().trim_end_matches('/').to_owned()
        };
        Ok(SyncConfig {
            mode: self.mode,
            webdav_url,
            webdav_username: self.webdav_username.trim().to_owned(),
            webdav_file_name: sanitize_remote_filename(&self.webdav_file_name)?,
            webdav_allow_invalid_certs: self.webdav_allow_invalid_certs,
            ..SyncConfig::default()
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncOutcome {
    pub local_changed: bool,
    pub added: usize,
    pub updated: usize,
    pub deleted: usize,
    pub uploaded: bool,
    pub pulled: bool,
    pub synced_at: u64,
    pub summary: String,
    pub vault_data: VaultPublicData,
    pub config: SyncConfigPublic,
}

fn webdav_password_save(account: &str, password: &str) -> Result<()> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, account)
        .map_err(|e| anyhow!("鑰匙圈初始化失敗：{e}"))?;
    entry
        .set_password(password)
        .map_err(|e| anyhow!("寫入 WebDAV 密碼失敗：{e}"))?;
    Ok(())
}

fn webdav_password_load(account: &str) -> Option<Zeroizing<String>> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, account).ok()?;
    entry
        .get_password()
        .ok()
        .filter(|s| !s.is_empty())
        .map(Zeroizing::new)
}

fn webdav_password_delete(account: &str) -> Result<()> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, account)
        .map_err(|e| anyhow!("鑰匙圈初始化失敗：{e}"))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(anyhow!("移除 WebDAV 密碼失敗：{e}")),
    }
}

pub struct SyncManager {
    config_path: PathBuf,
    account: String,
    config: SyncConfig,
}

impl SyncManager {
    pub fn copy_legacy_config(from: &std::path::Path, to: &std::path::Path) -> Result<bool> {
        let source = from.join(SYNC_CONFIG_FILE_NAME);
        if !source.try_exists()? {
            return Ok(false);
        }
        let raw = fs::read_to_string(source)?;
        let cfg: SyncConfig = serde_json::from_str(&raw)?;
        if cfg.mode != SyncMode::WebDav || validate_webdav_url(&cfg.webdav_url).is_err() {
            return Ok(false);
        }
        write_atomic_text(&to.join(SYNC_CONFIG_FILE_NAME), &raw)?;
        Ok(true)
    }

    pub fn new(config_dir: PathBuf, profile_id: &str) -> Self {
        let _ = fs::create_dir_all(&config_dir);
        let config_path = config_dir.join(SYNC_CONFIG_FILE_NAME);
        let config = fs::read_to_string(&config_path)
            .ok()
            .and_then(|s| serde_json::from_str::<SyncConfig>(&s).ok())
            .unwrap_or_default();
        Self {
            config_path,
            account: if profile_id == "legacy-webdav" { KEYCHAIN_WEBDAV_ACCOUNT.to_owned() } else { format!("{KEYCHAIN_WEBDAV_ACCOUNT}:{profile_id}") },
            config,
        }
    }

    pub fn config_clone(&self) -> SyncConfig {
        self.config.clone()
    }

    pub fn stored_password(&self) -> Option<Zeroizing<String>> {
        webdav_password_load(&self.account)
    }

    pub fn discard_unregistered(&self) {
        if self.stored_password().is_some() { let _ = webdav_password_delete(&self.account); }
        let _ = fs::remove_file(&self.config_path);
    }

    pub fn public_config(&self) -> SyncConfigPublic {
        Self::to_public(&self.config, self.stored_password().is_some())
    }

    fn to_public(cfg: &SyncConfig, has_webdav_password: bool) -> SyncConfigPublic {
        SyncConfigPublic {
            mode: cfg.mode,
            webdav_url: cfg.webdav_url.clone(),
            webdav_username: cfg.webdav_username.clone(),
            webdav_file_name: cfg.webdav_file_name.clone(),
            webdav_allow_invalid_certs: cfg.webdav_allow_invalid_certs,
            has_webdav_password,
            last_sync_at: cfg.last_sync_at,
            last_sync_status: cfg.last_sync_status.clone(),
        }
    }

    fn same_account(&self, cfg: &SyncConfig) -> bool {
        validate_webdav_url(&self.config.webdav_url).ok().as_deref() == Some(&cfg.webdav_url)
            && self.config.webdav_username == cfg.webdav_username
            && self.config.webdav_allow_invalid_certs == cfg.webdav_allow_invalid_certs
    }

    pub fn connection_password(&self, cfg: &SyncConfig, input: &SaveSyncConfigInput) -> Option<Zeroizing<String>> {
        if input.clear_webdav_password || cfg.webdav_username.is_empty() {
            return None;
        }
        input.webdav_password.as_ref().filter(|pwd| !pwd.is_empty()).cloned().map(Zeroizing::new)
            .or_else(|| self.same_account(cfg).then(|| self.stored_password()).flatten())
    }

    pub fn save_config(&mut self, input: SaveSyncConfigInput) -> Result<SyncConfigPublic> {
        let mut cfg = input.connection_config()?;
        if self.same_account(&cfg) && self.config.webdav_file_name == cfg.webdav_file_name {
            cfg.last_sync_at = self.config.last_sync_at;
            cfg.last_sync_status = self.config.last_sync_status.clone();
        }
        let previous_password = self.stored_password();
        let next_password = self.connection_password(&cfg, &input);
        let credentials_changed = previous_password.as_deref() != next_password.as_deref();
        if credentials_changed {
            match next_password.as_deref() {
                Some(password) => webdav_password_save(&self.account, password)?,
                None => webdav_password_delete(&self.account)?,
            }
        }
        let text = serde_json::to_string_pretty(&cfg).context("序列化同步設定失敗")?;
        if let Err(error) = write_atomic_text(&self.config_path, &text) {
            if credentials_changed {
                match previous_password.as_deref() {
                    Some(password) => webdav_password_save(&self.account, password),
                    None => webdav_password_delete(&self.account),
                }.context("同步設定儲存失敗，且無法還原系統鑰匙圈內的密碼")?;
            }
            return Err(error);
        }
        self.config = cfg;
        Ok(Self::to_public(&self.config, next_password.is_some()))
    }

    pub fn record_sync_result(&mut self, synced_at: u64, status: String) -> SyncConfigPublic {
        self.config.last_sync_at = Some(synced_at);
        self.config.last_sync_status = status;
        let _ = self.persist();
        self.public_config()
    }

    fn persist(&self) -> Result<()> {
        let text = serde_json::to_string_pretty(&self.config).context("序列化同步設定失敗")?;
        write_atomic_text(&self.config_path, &text)
    }
}

fn is_local_or_private_host(host: &str) -> bool {
    let h = host.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();
    if h == "localhost"
        || h.ends_with(".local")
        || h.ends_with(".lan")
        || h.ends_with(".internal")
        || h.ends_with(".home.arpa")
    {
        return true;
    }
    if let Ok(ip) = h.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_private() || v4.is_link_local()
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()
                    || (v6.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7 ULA
                    || (v6.segments()[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
            }
        };
    }
    false
}

pub fn validate_webdav_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("請輸入 WebDAV 網址"));
    }
    if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(anyhow!("WebDAV 網址不可包含空白或控制字元"));
    }
    let parsed = reqwest::Url::parse(trimmed).map_err(|_| anyhow!("WebDAV 網址格式不正確"))?;
    let scheme = parsed.scheme();
    if scheme != "https" && scheme != "http" {
        return Err(anyhow!("WebDAV 網址必須以 https:// 或 http:// 開頭"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(anyhow!("WebDAV 網址不可內嵌帳號密碼，請改填於下方欄位"));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(anyhow!("WebDAV 網址不可包含查詢參數（?）或片段（#）"));
    }
    let host = parsed
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| anyhow!("WebDAV 網址缺少主機名稱"))?;
    if scheme == "http" && !is_local_or_private_host(host) {
        return Err(anyhow!(
            "公開網路 WebDAV 必須使用 https://（僅本機或區域網路允許 http://）"
        ));
    }
    Ok(trimmed.trim_end_matches('/').to_string())
}

pub fn sanitize_remote_filename(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(DEFAULT_SYNC_FILE_NAME.to_string());
    }
    if trimmed.len() > 128 {
        return Err(anyhow!("同步檔名過長（上限 128 字元）"));
    }
    if trimmed.starts_with('.')
        || trimmed.contains("..")
        || !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(anyhow!(
            "同步檔名僅限英數字、點、底線與連字號（不可包含空白或特殊符號）"
        ));
    }
    Ok(trimmed.to_string())
}

fn build_webdav_target_url(base_url: &str, file_name: &str) -> Result<String> {
    let base = validate_webdav_url(base_url)?;
    let fname = sanitize_remote_filename(file_name)?;
    if base.to_ascii_lowercase().ends_with(".json") {
        Ok(base)
    } else {
        Ok(format!("{base}/{fname}"))
    }
}

fn build_http_client(allow_invalid_certs: bool) -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .danger_accept_invalid_certs(allow_invalid_certs)
        .user_agent("MistRDP/0.2")
        .build()
        .context("建立 HTTP 連線端失敗")
}

pub async fn test_sync_connection(
    cfg: &SyncConfig,
    webdav_password_override: Option<Zeroizing<String>>,
) -> Result<String> {
    match cfg.mode {
        SyncMode::Disabled => Err(anyhow!("同步功能尚未啟用")),
        SyncMode::WebDav => {
            let target_url = build_webdav_target_url(&cfg.webdav_url, &cfg.webdav_file_name)?;
            let client = build_http_client(cfg.webdav_allow_invalid_certs)?;
            let pwd = webdav_password_override;

            let mut req = client.get(&target_url);
            if !cfg.webdav_username.is_empty() {
                req = req.basic_auth(
                    &cfg.webdav_username,
                    pwd.as_deref().map(|s| s.as_str()),
                );
            }
            let resp = req.send().await.context("無法連線至 WebDAV 伺服器")?;
            match resp.status() {
                StatusCode::OK => Ok("WebDAV 連線成功（已偵測到遠端加密同步檔）".to_string()),
                StatusCode::NOT_FOUND => {
                    Ok("WebDAV 連線成功（首次同步將建立遠端加密檔）".to_string())
                }
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    Err(anyhow!("WebDAV 認證失敗，請檢查帳號與密碼"))
                }
                code if code.is_redirection() => {
                    Err(anyhow!("WebDAV 伺服器回傳重導向（{code}），請填入最終 HTTPS 網址"))
                }
                code => Err(anyhow!("WebDAV 伺服器回傳異常狀態碼：{code}")),
            }
        }
    }
}

async fn download_webdav(
    client: &Client,
    target_url: &str,
    cfg: &SyncConfig,
    password: Option<&String>,
) -> Result<(Option<String>, Option<String>)> {
    let mut request = client.get(target_url);
    if !cfg.webdav_username.is_empty() {
        request = request.basic_auth(&cfg.webdav_username, password);
    }
    let mut response = request.send().await.context("下載 WebDAV 同步檔失敗")?;
    match response.status() {
        StatusCode::OK => {}
        StatusCode::NOT_FOUND => return Ok((None, None)),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            return Err(anyhow!("WebDAV 認證失敗，請檢查帳號與密碼"));
        }
        status if status.is_redirection() => {
            return Err(anyhow!("WebDAV 伺服器回傳重導向（{status}），請填入最終網址"));
        }
        status => return Err(anyhow!("WebDAV 下載失敗，狀態碼：{status}")),
    }
    let etag = response.headers().get(ETAG).and_then(|v| v.to_str().ok())
        .filter(|v| v.len() >= 2 && v.starts_with('"') && v.ends_with('"')).map(str::to_owned);
    if response.content_length().is_some_and(|n| n > MAX_REMOTE_VAULT_BYTES as u64) {
        return Err(anyhow!("遠端同步檔案超過大小上限（10 MB）"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.context("讀取 WebDAV 回應內容失敗")? {
        if bytes.len() + chunk.len() > MAX_REMOTE_VAULT_BYTES {
            return Err(anyhow!("遠端同步檔案超過大小上限（10 MB）"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let text = String::from_utf8(bytes).context("遠端同步檔不是有效 UTF-8")?;
    Ok((Some(text), etag))
}

pub async fn fetch_webdav_vault(cfg: &SyncConfig, password: Option<&String>) -> Result<String> {
    let target_url = build_webdav_target_url(&cfg.webdav_url, &cfg.webdav_file_name)?;
    let client = build_http_client(cfg.webdav_allow_invalid_certs)?;
    download_webdav(&client, &target_url, cfg, password).await?.0
        .ok_or_else(|| anyhow!("伺服器上找不到保管庫，請檢查檔名或選擇建立 WebDAV 保管庫"))
}

pub async fn create_webdav_vault(cfg: &SyncConfig, password: Option<&String>, encrypted: String) -> Result<()> {
    let url = cfg.target_url()?;
    let client = build_http_client(cfg.webdav_allow_invalid_certs)?;
    if download_webdav(&client, &url, cfg, password).await?.0.is_some() {
        return Err(anyhow!("遠端已有同名保管庫，請改用連線既有保管庫"));
    }
    let mut request = client.put(url).header(IF_NONE_MATCH, "*")
        .header(CONTENT_TYPE, "application/json; charset=utf-8").body(encrypted);
    if !cfg.webdav_username.is_empty() {
        request = request.basic_auth(&cfg.webdav_username, password);
    }
    let status = request.send().await.context("建立遠端 WebDAV 保管庫失敗")?.status();
    match status {
        StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => Ok(()),
        StatusCode::PRECONDITION_FAILED => Err(anyhow!("遠端已有同名保管庫，請改用連線既有保管庫")),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(anyhow!("WebDAV 寫入權限不足或認證失敗")),
        _ => Err(anyhow!("建立遠端保管庫失敗，狀態碼：{status}")),
    }
}

fn format_merge_summary(stats: &MergeStats, uploaded: bool, pulled: bool) -> String {
    let mut parts = Vec::new();
    if stats.added > 0 {
        parts.push(format!("+{} 新增", stats.added));
    }
    if stats.updated > 0 {
        parts.push(format!("{} 更新", stats.updated));
    }
    if stats.deleted > 0 {
        parts.push(format!("{} 移除", stats.deleted));
    }
    if parts.is_empty() {
        if uploaded && !pulled {
            "已上傳本機保管庫".to_string()
        } else if uploaded {
            "已同步並更新遠端".to_string()
        } else {
            "已是最新狀態".to_string()
        }
    } else if uploaded {
        format!("已合併（{}）並更新遠端", parts.join(" / "))
    } else {
        format!("已合併（{}）", parts.join(" / "))
    }
}

async fn decrypt_remote_off_thread(
    remote_raw: String,
    master_password: Zeroizing<String>,
) -> Result<crate::vault::VaultPayload> {
    tokio::task::spawn_blocking(move || {
        let (payload, _, _) = decrypt_envelope_with_password(&remote_raw, &master_password)?;
        Ok(payload)
    })
    .await
    .map_err(|e| anyhow!("解密背景工作失敗：{e}"))?
}

pub async fn run_sync_with_vault(
    vault: &Mutex<VaultManager>,
    cfg: &SyncConfig,
    webdav_password_override: Option<Zeroizing<String>>,
) -> Result<(MergeStats, bool, bool, VaultPublicData)> {
    match cfg.mode {
        SyncMode::Disabled => Err(anyhow!("同步功能尚未啟用")),
        SyncMode::WebDav => sync_webdav(vault, cfg, webdav_password_override).await,
    }
}

async fn sync_webdav(
    vault: &Mutex<VaultManager>,
    cfg: &SyncConfig,
    webdav_password_override: Option<Zeroizing<String>>,
) -> Result<(MergeStats, bool, bool, VaultPublicData)> {
    let target_url = build_webdav_target_url(&cfg.webdav_url, &cfg.webdav_file_name)?;
    let client = build_http_client(cfg.webdav_allow_invalid_certs)?;
    let webdav_pwd = webdav_password_override;

    let mut total_stats = MergeStats::default();
    let mut any_pulled = false;

    for attempt in 0..MAX_WEBDAV_RETRIES {
        let master_pwd = {
            let v = vault.lock().map_err(|e| anyhow!("{e}"))?;
            v.master_password_clone()?
        };

        let (remote_raw, etag) = download_webdav(&client, &target_url, cfg, webdav_pwd.as_deref()).await?;

        let remote_payload = match remote_raw {
            Some(raw) => Some(
                decrypt_remote_off_thread(raw, master_pwd)
                    .await
                    .context("解密遠端 WebDAV 保管庫失敗（請確認各裝置保管庫密碼一致）")?,
            ),
            None => None,
        };

        let (stats, merged_json, pub_data, should_upload, pulled) = {
            let mut v = vault.lock().map_err(|e| anyhow!("{e}"))?;
            if let Some(ref remote) = remote_payload {
                let (st, json, pub_d) = v.apply_remote_merge(remote)?;
                let need_up = st.remote_needs_update;
                (st, json, pub_d, need_up, true)
            } else {
                let json = v.build_sync_envelope_json()?;
                let pub_d = v.public_data()?;
                let st = MergeStats {
                    remote_needs_update: true,
                    ..MergeStats::default()
                };
                (st, json, pub_d, true, false)
            }
        };

        total_stats.added += stats.added;
        total_stats.updated += stats.updated;
        total_stats.deleted += stats.deleted;
        total_stats.local_changed |= stats.local_changed;
        total_stats.remote_needs_update |= stats.remote_needs_update;
        any_pulled |= pulled;

        if !should_upload {
            return Ok((total_stats, false, any_pulled, pub_data));
        }
        if merged_json.len() > MAX_REMOTE_VAULT_BYTES {
            return Err(anyhow!("保管庫超過 WebDAV 同步大小上限（10 MB）"));
        }

        let mut put_req = client
            .put(&target_url)
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .body(merged_json);
        if !cfg.webdav_username.is_empty() {
            put_req = put_req.basic_auth(
                &cfg.webdav_username,
                webdav_pwd.as_deref().map(|s| s.as_str()),
            );
        }
        match (pulled, etag) {
            (_, Some(etag)) => put_req = put_req.header(IF_MATCH, etag),
            (false, None) => put_req = put_req.header(IF_NONE_MATCH, "*"),
            (true, None) => return Err(anyhow!("WebDAV 未回傳可用的 ETag，已停止上傳以避免覆蓋遠端資料")),
        }

        let put_resp = put_req.send().await.context("上傳至 WebDAV 失敗")?;
        match put_resp.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => {
                // Re-read latest public_data after async PUT so concurrent local edits are never masked in UI.
                let latest_pub = vault
                    .lock()
                    .ok()
                    .and_then(|v| v.public_data().ok())
                    .unwrap_or(pub_data);
                return Ok((total_stats, true, any_pulled, latest_pub));
            }
            StatusCode::PRECONDITION_FAILED => {
                if attempt + 1 < MAX_WEBDAV_RETRIES {
                    tracing::info!("WebDAV ETag conflict (412), retrying pull-merge-push...");
                    continue;
                }
                return Err(anyhow!("WebDAV 並發衝突過多，請稍後重試"));
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(anyhow!("WebDAV 寫入權限不足或認證失敗"));
            }
            code => {
                return Err(anyhow!("WebDAV 上傳失敗，狀態碼：{code}"));
            }
        }
    }

    Err(anyhow!("WebDAV 並發衝突過多，請稍後重試"))
}

pub fn build_sync_outcome(
    stats: MergeStats,
    uploaded: bool,
    pulled: bool,
    vault_data: VaultPublicData,
    sync_mgr: &Mutex<SyncManager>,
) -> SyncOutcome {
    let synced_at = now_epoch_ms();
    let summary = format_merge_summary(&stats, uploaded, pulled);
    let config = sync_mgr
        .lock()
        .map(|mut m| m.record_sync_result(synced_at, summary.clone()))
        .unwrap_or_else(|_| SyncConfigPublic {
            mode: SyncMode::Disabled,
            webdav_url: String::new(),
            webdav_username: String::new(),
            webdav_file_name: default_sync_filename(),
            webdav_allow_invalid_certs: false,
            has_webdav_password: false,
            last_sync_at: Some(synced_at),
            last_sync_status: summary.clone(),
        });
    SyncOutcome {
        local_changed: stats.local_changed,
        added: stats.added,
        updated: stats.updated,
        deleted: stats.deleted,
        uploaded,
        pulled,
        synced_at,
        summary,
        vault_data,
        config,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::SavedHost;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn mk_temp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "mistrdp_sync_test_{}_{}",
            tag,
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn mk_host(id: &str, name: &str, addr: &str) -> SavedHost {
        SavedHost {
            id: id.into(),
            name: name.into(),
            address: addr.into(),
            port: 3389,
            group: "預設".into(),
            group_id: None,
            username: "admin".into(),
            domain: String::new(),
            save_password: true,
            password: Some("HostSecret#99".into()),
            account_id: None,
            width: 0,
            height: 0,
            clipboard_enabled: true,
            keyboard_hook_enabled: true,
            audio_enabled: true,
            shared_folder: String::new(),
            updated_at: 0,
            deleted_at: None,
        }
    }

    #[test]
    fn webdav_url_and_filename_validation() {
        assert!(validate_webdav_url("https://nas.local:5006/dav/mistrdp/").is_ok());
        assert!(validate_webdav_url("https://cloud.example.com/remote.php/dav/files/u").is_ok());
        assert!(validate_webdav_url("http://192.168.1.50/webdav").is_ok());
        assert!(validate_webdav_url("http://127.0.0.1:8080/dav").is_ok());
        assert!(validate_webdav_url("http://nas.local:5005/dav").is_ok());

        // Reject embedded credentials, public plain HTTP, query/fragment, invalid schemes
        assert!(validate_webdav_url("https://user:secret@nas.local/dav").is_err());
        assert!(validate_webdav_url("https://user@nas.local/dav").is_err());
        assert!(validate_webdav_url("http://public.example.com/dav").is_err());
        assert!(validate_webdav_url("https://nas.local/dav?foo=1").is_err());
        assert!(validate_webdav_url("https://nas.local/dav#frag").is_err());
        assert!(validate_webdav_url("file:///etc/passwd").is_err());
        assert!(validate_webdav_url("ftp://nas.local").is_err());
        assert!(validate_webdav_url("https://nas.local/bad path").is_err());
        assert!(validate_webdav_url("https://").is_err());

        assert_eq!(sanitize_remote_filename("").unwrap(), "mist-rdp.vault.json");
        assert_eq!(sanitize_remote_filename("my-vault_2.json").unwrap(), "my-vault_2.json");
        assert!(sanitize_remote_filename("my vault.json").is_err());
        assert!(sanitize_remote_filename("a?b.json").is_err());
        assert!(sanitize_remote_filename("a#b.json").is_err());
        assert!(sanitize_remote_filename("a%20b.json").is_err());
        assert!(sanitize_remote_filename(".hidden.json").is_err());
        assert!(sanitize_remote_filename("../evil.json").is_err());
        assert!(sanitize_remote_filename("sub/file.json").is_err());
        assert!(sanitize_remote_filename("sub\\file.json").is_err());
    }

    #[tokio::test]
    async fn webdav_creation_refuses_a_competing_create() {
        let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap();
        let service = tokio::spawn(async move {
            for response_code in ["404 Not Found","412 Precondition Failed"] {
                let (mut socket,_) = server.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let byte = socket.read_u8().await.unwrap();
                    request.push(byte);
                    if request.ends_with(b"\r\n\r\n") { break; }
                }
                let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                if response_code.starts_with("404") {
                    assert!(request.starts_with("get /dav/mist-rdp.vault.json "));
                } else {
                    assert!(request.starts_with("put /dav/mist-rdp.vault.json "));
                    assert!(request.contains("\r\nif-none-match: *\r\n"));
                    let mut body = [0u8;2];
                    socket.read_exact(&mut body).await.unwrap();
                    assert_eq!(&body,b"{}");
                }
                socket.write_all(format!("HTTP/1.1 {response_code}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            }
        });
        let config = SyncConfig { mode:SyncMode::WebDav, webdav_url:format!("http://{address}/dav"), ..SyncConfig::default() };
        let result = create_webdav_vault(&config,None,"{}".into()).await.unwrap_err();
        assert!(result.to_string().contains("已有同名保管庫"));
        service.await.unwrap();
    }

    #[tokio::test]
    async fn webdav_sync_with_etag_412_conflict_retry_converges() {
        let dir = mk_temp_dir("webdav_412");
        let dev_a = Mutex::new(VaultManager::with_exact_path(dir.join("a.vault.json")));
        let dev_b = Mutex::new(VaultManager::with_exact_path(dir.join("b.vault.json")));

        {
            let mut a = dev_a.lock().unwrap();
            a.create("DavMaster#2026").unwrap();
            a.save_host(mk_host("ha", "Alpha", "10.1.0.1")).unwrap();
        }
        let remote_v1_json = dev_a.lock().unwrap().build_sync_envelope_json().unwrap();

        // Simulate concurrent Device B pushing an update that bumps remote ETag from "v1" -> "v2"
        {
            let mut b = dev_b.lock().unwrap();
            b.create("DavMaster#2026").unwrap();
            b.save_host(mk_host("hb", "Beta", "10.1.0.2")).unwrap();
        }
        let remote_v2_json = dev_b.lock().unwrap().build_sync_envelope_json().unwrap();

        // Now Device A adds a new host locally and tries to sync while server races on first PUT
        dev_a
            .lock()
            .unwrap()
            .save_host(mk_host("ha2", "AlphaTwo", "10.1.0.3"))
            .unwrap();

        struct MockState {
            body: String,
            etag: String,
            put_count: usize,
            race_body: String,
        }
        let server_state = Arc::new(Mutex::new(MockState {
            body: remote_v1_json,
            etag: "\"v1\"".to_string(),
            put_count: 0,
            race_body: remote_v2_json,
        }));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv_state = server_state.clone();

        let server_task = tokio::spawn(async move {
            // Serve up to 6 requests
            for _ in 0..6 {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 65536];
                let mut total = 0usize;
                loop {
                    let n = sock.read(&mut buf[total..]).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    total += n;
                    if let Some(hdr_end) = buf[..total].windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers_str = String::from_utf8_lossy(&buf[..hdr_end]);
                        let content_len: usize = headers_str
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                if k.eq_ignore_ascii_case("content-length") {
                                    v.trim().parse().ok()
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(0);
                        if total >= hdr_end + 4 + content_len {
                            break;
                        }
                    }
                }
                let req_str = String::from_utf8_lossy(&buf[..total]);
                let first_line = req_str.lines().next().unwrap_or("");
                let method = first_line.split_whitespace().next().unwrap_or("");
                let body_part = req_str
                    .split_once("\r\n\r\n")
                    .map(|(_, b)| b.to_string())
                    .unwrap_or_default();

                let resp = {
                    let mut st = srv_state.lock().unwrap();
                    if method == "GET" {
                        format!(
                            "HTTP/1.1 200 OK\r\nETag: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            st.etag,
                            st.body.len(),
                            st.body
                        )
                    } else if method == "PUT" {
                        st.put_count += 1;
                        if st.put_count == 1 {
                            // Simulate concurrent write from Device B right before A's PUT
                            st.body = st.race_body.clone();
                            st.etag = "\"v2\"".to_string();
                            "HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                        } else {
                            st.body = body_part;
                            st.etag = "\"v3\"".to_string();
                            "HTTP/1.1 204 No Content\r\nETag: \"v3\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                        }
                    } else {
                        "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                    }
                };
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        let cfg = SyncConfig {
            mode: SyncMode::WebDav,
            webdav_url: format!("http://{addr}/dav"),
            webdav_username: "user".into(),
            webdav_file_name: "mist-rdp.vault.json".into(),
            ..SyncConfig::default()
        };

        let (stats, uploaded, pulled, pub_data) =
            run_sync_with_vault(&dev_a, &cfg, Some(Zeroizing::new("pass".into())))
                .await
                .unwrap();

        assert!(uploaded);
        assert!(pulled);
        assert_eq!(stats.added, 1); // hb from Device B pulled during 412 retry!
        assert_eq!(pub_data.hosts.len(), 3); // ha + ha2 + hb all preserved!
        assert_eq!(server_state.lock().unwrap().put_count, 2);

        server_task.abort();
        let _ = fs::remove_dir_all(&dir);
    }
}
