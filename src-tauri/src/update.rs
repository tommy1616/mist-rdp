//! Self-update from GitHub Releases.
//!
//! Flow: query the latest Release -> compare versions -> download the matching asset ourselves
//! (a download made by the app carries no macOS quarantine flag) -> verify the SHA-256 that GitHub
//! publishes for the asset -> swap the running program for the new one and relaunch.
//! Any failure before the swap leaves the installed version untouched.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

const REPO: &str = "tommy1616/mist-rdp";
const API_ENV: &str = "MISTRDP_UPDATE_API";
const MAX_ASSET_BYTES: u64 = 300 * 1024 * 1024;

#[cfg(windows)]
const ASSET_SUFFIX: &str = "windows-x64.exe";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ASSET_SUFFIX: &str = "macos-arm64.zip";
#[cfg(not(any(windows, all(target_os = "macos", target_arch = "aarch64"))))]
const ASSET_SUFFIX: &str = "";

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize, Clone)]
struct GhAsset {
    name: String,
    size: u64,
    #[serde(default)]
    digest: Option<String>,
    browser_download_url: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
    pub available: bool,
    pub notes: String,
    pub asset_name: String,
    pub size: u64,
    /// False when this platform has no matching asset or GitHub published no checksum.
    pub installable: bool,
}

fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Loopback override used only for local testing; everything else talks to GitHub over HTTPS.
fn is_loopback_http(url: &str) -> bool {
    match reqwest::Url::parse(url) {
        Ok(u) => {
            u.scheme() == "http"
                && u.port().is_some()
                && u.username().is_empty()
                && u.password().is_none()
                && matches!(u.host_str(), Some("127.0.0.1") | Some("localhost"))
        }
        Err(_) => false,
    }
}

fn api_override() -> Option<String> {
    let v = std::env::var(API_ENV).ok()?;
    let v = v.trim().to_string();
    if is_loopback_http(&v) {
        Some(v)
    } else {
        None
    }
}

fn parse_version(text: &str) -> Option<Vec<u64>> {
    let t = text.trim().trim_start_matches(['v', 'V']);
    let t = t.split(['-', '+']).next()?;
    let parts: Option<Vec<u64>> = t.split('.').map(|p| p.parse::<u64>().ok()).collect();
    let parts = parts?;
    if parts.is_empty() {
        None
    } else {
        Some(parts)
    }
}

fn is_newer(latest: &str, current: &str) -> bool {
    let (Some(mut a), Some(mut b)) = (parse_version(latest), parse_version(current)) else {
        return false;
    };
    let n = a.len().max(b.len());
    a.resize(n, 0);
    b.resize(n, 0);
    a > b
}

fn parse_digest(digest: &str) -> Option<String> {
    let hex = digest.strip_prefix("sha256:")?;
    if hex.len() == 64 && hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        Some(hex.to_ascii_lowercase())
    } else {
        None
    }
}

fn pick_asset(assets: &[GhAsset]) -> Option<&GhAsset> {
    if ASSET_SUFFIX.is_empty() {
        return None;
    }
    assets.iter().find(|a| a.name.starts_with("MistRDP-") && a.name.ends_with(ASSET_SUFFIX))
}

fn download_url_allowed(url: &str, overridden: bool) -> bool {
    if overridden {
        return is_loopback_http(url);
    }
    url.starts_with(&format!("https://github.com/{REPO}/releases/download/"))
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(format!("MistRDP/{}", current_version()))
        .connect_timeout(Duration::from_secs(10))
        .build()?)
}

async fn fetch_latest() -> Result<(GhRelease, bool)> {
    let override_base = api_override();
    let url = override_base
        .clone()
        .unwrap_or_else(|| format!("https://api.github.com/repos/{REPO}/releases/latest"));
    let resp = client()?
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .context("無法連線到 GitHub")?;
    if !resp.status().is_success() {
        bail!("GitHub 回應 {}", resp.status());
    }
    let bytes = resp.bytes().await.context("讀取版本資訊失敗")?;
    let release: GhRelease = serde_json::from_slice(&bytes).context("無法解析版本資訊")?;
    Ok((release, override_base.is_some()))
}

fn info_from(release: &GhRelease) -> UpdateInfo {
    let asset = pick_asset(&release.assets);
    let installable = asset
        .map(|a| a.digest.as_deref().and_then(parse_digest).is_some())
        .unwrap_or(false);
    UpdateInfo {
        current: current_version().to_string(),
        latest: release.tag_name.trim_start_matches(['v', 'V']).to_string(),
        available: is_newer(&release.tag_name, current_version()),
        notes: release.body.chars().take(4000).collect(),
        asset_name: asset.map(|a| a.name.clone()).unwrap_or_default(),
        size: asset.map(|a| a.size).unwrap_or(0),
        installable,
    }
}

pub async fn check() -> Result<UpdateInfo> {
    let (release, _) = fetch_latest().await?;
    Ok(info_from(&release))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn download_verified(app: &AppHandle, asset: &GhAsset, expected: &str, overridden: bool, dest: &Path) -> Result<()> {
    if !download_url_allowed(&asset.browser_download_url, overridden) {
        bail!("下載網址不在允許範圍內");
    }
    if asset.size == 0 || asset.size > MAX_ASSET_BYTES {
        bail!("更新檔大小異常");
    }
    let mut resp = client()?
        .get(&asset.browser_download_url)
        .timeout(Duration::from_secs(600))
        .send()
        .await
        .context("下載更新失敗")?;
    if !resp.status().is_success() {
        bail!("下載回應 {}", resp.status());
    }
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::File::create(dest).await?;
    let mut hasher = Sha256::new();
    let mut done: u64 = 0;
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    while let Some(chunk) = resp.chunk().await.context("下載中斷")? {
        done += chunk.len() as u64;
        if done > MAX_ASSET_BYTES {
            bail!("更新檔超過大小上限");
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
        if last_emit.elapsed() >= Duration::from_millis(150) {
            last_emit = Instant::now();
            let _ = app.emit("update-progress", serde_json::json!({ "done": done, "total": asset.size }));
        }
    }
    file.flush().await?;
    drop(file);
    let _ = app.emit("update-progress", serde_json::json!({ "done": done, "total": asset.size }));
    if done != asset.size {
        bail!("下載大小不符（{done} / {}）", asset.size);
    }
    let actual = hex(&hasher.finalize());
    if actual != expected {
        let _ = std::fs::remove_file(dest);
        bail!("SHA-256 驗證失敗，已放棄更新");
    }
    Ok(())
}

fn work_dir() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("mistrdp-update-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub async fn install(app: AppHandle) -> Result<()> {
    let (release, overridden) = fetch_latest().await?;
    let info = info_from(&release);
    if !info.available {
        bail!("目前已是最新版本");
    }
    let asset = pick_asset(&release.assets)
        .cloned()
        .ok_or_else(|| anyhow!("這個平台沒有對應的更新檔"))?;
    let expected = asset
        .digest
        .as_deref()
        .and_then(parse_digest)
        .ok_or_else(|| anyhow!("GitHub 沒有提供這個檔案的校驗碼，不自動更新"))?;
    if Path::new(&asset.name).file_name().map(|n| n.to_string_lossy() != asset.name.as_str()).unwrap_or(true) {
        bail!("更新檔名稱不合法");
    }
    let dir = work_dir()?;
    let download = dir.join(&asset.name);
    let result = async {
        download_verified(&app, &asset, &expected, overridden, &download).await?;
        let _ = app.emit("update-progress", serde_json::json!({ "stage": "applying" }));
        apply(&download, &dir)
    }
    .await;
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }
    // Give the frontend a moment to show the restart notice, then leave so the swap can finish.
    tokio::time::sleep(Duration::from_millis(400)).await;
    app.exit(0);
    Ok(())
}

#[cfg(windows)]
fn old_exe_path(exe: &Path) -> PathBuf {
    let mut name = exe.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".old");
    exe.with_file_name(name)
}

/// Remove the leftover from a previous in-place update. Called at startup.
pub fn cleanup_after_update() {
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_exe_path(&exe));
    }
}

#[cfg(windows)]
fn apply(downloaded: &Path, _dir: &Path) -> Result<()> {
    let exe = std::env::current_exe().context("找不到目前的程式路徑")?;
    let old = old_exe_path(&exe);
    let _ = std::fs::remove_file(&old);
    // A running exe cannot be overwritten but can be renamed.
    std::fs::rename(&exe, &old).context("無法替換程式，請確認資料夾有寫入權限（不要放在 Program Files）")?;
    if let Err(e) = std::fs::copy(downloaded, &exe) {
        let _ = std::fs::remove_file(&exe);
        let _ = std::fs::rename(&old, &exe);
        return Err(anyhow!("寫入新版本失敗，已還原舊版：{e}"));
    }
    let restore = |why: String| -> anyhow::Error {
        let _ = std::fs::remove_file(&exe);
        let _ = std::fs::rename(&old, &exe);
        anyhow!("{why}，已還原舊版")
    };
    let mut child = match std::process::Command::new(&exe).spawn() {
        Ok(c) => c,
        Err(e) => return Err(restore(format!("無法啟動新版本：{e}"))),
    };
    // A build that starts and dies at once must not strand the user without a working program.
    std::thread::sleep(Duration::from_millis(2500));
    match child.try_wait() {
        Ok(None) => {}
        Ok(Some(status)) => return Err(restore(format!("新版本啟動後立刻結束（{status}）"))),
        Err(e) => return Err(restore(format!("無法確認新版本狀態：{e}"))),
    }
    let _ = std::fs::remove_file(downloaded);
    Ok(())
}

#[cfg(target_os = "macos")]
fn apply(downloaded: &Path, dir: &Path) -> Result<()> {
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe().context("找不到目前的程式路徑")?;
    let app_dir = exe
        .ancestors()
        .nth(3)
        .filter(|p| p.extension().map(|e| e == "app").unwrap_or(false))
        .ok_or_else(|| anyhow!("目前不是從 .app 執行，無法自動更新"))?
        .to_path_buf();
    let extract = dir.join("extract");
    std::fs::create_dir_all(&extract)?;
    let status = Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(downloaded)
        .arg(&extract)
        .status()
        .context("無法解壓縮更新檔")?;
    if !status.success() {
        bail!("解壓縮更新檔失敗");
    }
    let new_app = std::fs::read_dir(&extract)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().map(|e| e == "app").unwrap_or(false))
        .ok_or_else(|| anyhow!("更新檔裡找不到 App"))?;
    let status = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&new_app)
        .status()
        .context("無法檢查新版簽章")?;
    if !status.success() {
        bail!("新版 App 簽章檢查失敗，已放棄更新");
    }
    // The app folder's parent must be writable for the swap.
    let parent = app_dir.parent().ok_or_else(|| anyhow!("找不到 App 所在位置"))?;
    let probe = parent.join(format!(".mistrdp-write-test-{}", std::process::id()));
    std::fs::File::create(&probe).context("App 所在資料夾沒有寫入權限，無法自動更新")?;
    let _ = std::fs::remove_file(&probe);

    let script = dir.join("swap.sh");
    std::fs::write(
        &script,
        r#"#!/bin/sh
PID="$1"; OLD="$2"; NEW="$3"; BAK="$OLD.old-update"
i=0
while kill -0 "$PID" 2>/dev/null; do
  sleep 0.3; i=$((i+1)); [ "$i" -gt 100 ] && break
done
rm -rf "$BAK"
mv "$OLD" "$BAK" || { open "$OLD"; exit 1; }
if /usr/bin/ditto "$NEW" "$OLD"; then
  /usr/bin/xattr -dr com.apple.quarantine "$OLD" 2>/dev/null
  rm -rf "$BAK"
else
  rm -rf "$OLD"; mv "$BAK" "$OLD"
fi
open "$OLD"
rm -rf "$(dirname "$(dirname "$NEW")")"
"#,
    )?;
    Command::new("/bin/sh")
        .arg(&script)
        .arg(std::process::id().to_string())
        .arg(&app_dir)
        .arg(&new_app)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("無法啟動更新程序")?;
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn apply(_downloaded: &Path, _dir: &Path) -> Result<()> {
    bail!("這個平台尚不支援自動更新")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_numerically() {
        assert!(is_newer("v0.2.19", "0.2.9"));
        assert!(!is_newer("v0.2.9", "0.2.19"));
        assert!(!is_newer("v0.2.19", "0.2.19"));
        assert!(is_newer("0.3.0", "0.2.99"));
        assert!(is_newer("v1.0", "0.9.9"));
        assert!(!is_newer("garbage", "0.2.19"));
    }

    #[test]
    fn digest_must_be_sha256_hex() {
        let good = format!("sha256:{}", "ab".repeat(32));
        assert!(parse_digest(&good).is_some());
        assert!(parse_digest("sha256:zz").is_none());
        assert!(parse_digest(&"ab".repeat(32)).is_none());
        assert!(parse_digest("sha1:abcd").is_none());
    }

    #[test]
    fn download_urls_are_restricted() {
        assert!(download_url_allowed("https://github.com/tommy1616/mist-rdp/releases/download/v1/x", false));
        assert!(!download_url_allowed("https://evil.example/x", false));
        assert!(!download_url_allowed("http://127.0.0.1:9/x", false));
        assert!(download_url_allowed("http://127.0.0.1:9/x", true));
        assert!(!download_url_allowed("https://evil.example/x", true));
        assert!(!is_loopback_http("http://localhost:1@evil.com/x"));
        assert!(!is_loopback_http("http://127.0.0.1/x"));
        assert!(is_loopback_http("http://localhost:8080/x"));
    }

    #[test]
    fn picks_matching_asset() {
        let mk = |n: &str| GhAsset { name: n.into(), size: 1, digest: None, browser_download_url: String::new() };
        let assets = vec![
            mk("MistRDP-v1-macos-arm64.dmg"),
            mk("MistRDP-v1-macos-arm64.zip"),
            mk("MistRDP-v1-windows-x64.exe"),
        ];
        let got = pick_asset(&assets).map(|a| a.name.clone());
        #[cfg(windows)]
        assert_eq!(got.as_deref(), Some("MistRDP-v1-windows-x64.exe"));
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        assert_eq!(got.as_deref(), Some("MistRDP-v1-macos-arm64.zip"));
        let _ = got;
    }
}
