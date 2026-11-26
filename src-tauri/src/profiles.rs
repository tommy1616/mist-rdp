use crate::sync::{SyncConfigPublic, SyncManager};
use crate::vault::{VaultManager, VaultStatus, write_atomic_text};
use anyhow::{Context, Result};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const LEGACY_ID: &str = "legacy-webdav";

pub struct WebDavProfile {
    pub id: String,
    pub vault: Mutex<VaultManager>,
    pub sync: Mutex<SyncManager>,
    pub gate: tokio::sync::Mutex<()>,
    name_path: PathBuf,
}

#[derive(Serialize)]
pub struct WebDavProfileView {
    pub id: String,
    pub name: String,
    pub status: VaultStatus,
    pub config: SyncConfigPublic,
}

impl WebDavProfile {
    fn new(root: &Path, id: String) -> Self {
        let path = root.join(&id);
        Self {
            vault: Mutex::new(VaultManager::for_remote(path.join("mist-rdp.vault.json"), &id)),
            sync: Mutex::new(SyncManager::new(path.clone(), &id)),
            gate: tokio::sync::Mutex::new(()),
            name_path: path.join("name.json"),
            id,
        }
    }

    pub fn set_name(&self, name: &str) -> Result<()> {
        write_atomic_text(&self.name_path, &serde_json::to_string(name)?)
    }

    pub fn view(&self) -> Result<WebDavProfileView, String> {
        let name = fs::read_to_string(&self.name_path).ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .ok_or_else(|| "無法讀取 WebDAV 保管庫名稱".to_string())?;
        let status = self.vault.lock().map_err(|e| e.to_string())?.status();
        let config = self.sync.lock().map_err(|e| e.to_string())?.public_config();
        Ok(WebDavProfileView { id: self.id.clone(), name, status, config })
    }
}

pub struct PendingProfile(Option<Arc<WebDavProfile>>);

impl PendingProfile {
    pub fn new(profile: Arc<WebDavProfile>) -> Self { Self(Some(profile)) }

    pub fn commit(&mut self) { self.0 = None; }
}

impl Drop for PendingProfile {
    fn drop(&mut self) {
        let Some(profile) = self.0.take() else { return; };
        if let Ok(mut vault) = profile.vault.lock() {
            let status = vault.status();
            if status.remember_keychain { let _ = vault.set_remember_keychain(false); }
            vault.lock();
            let _ = fs::remove_file(status.vault_path);
        }
        if let Ok(sync) = profile.sync.lock() { sync.discard_unregistered(); }
        let _ = fs::remove_file(&profile.name_path);
        if let Some(dir) = profile.name_path.parent() { let _ = fs::remove_dir(dir); }
    }
}

pub struct WebDavProfiles {
    root: PathBuf,
    entries: Mutex<Vec<Arc<WebDavProfile>>>,
    pub registration: tokio::sync::Mutex<()>,
}

impl WebDavProfiles {
    pub fn new(data_dir: &Path) -> Result<Self> {
        let root = data_dir.join("webdav-vaults");
        fs::create_dir_all(&root).context("無法建立 WebDAV 保管庫目錄")?;
        let legacy = root.join(LEGACY_ID);
        if !legacy.join("name.json").try_exists()? {
            if SyncManager::copy_legacy_config(data_dir, &legacy).unwrap_or_else(|error| {
                tracing::warn!(%error, "Legacy WebDAV settings could not be migrated");
                false
            }) {
                WebDavProfile::new(&root, LEGACY_ID.into()).set_name("原有 WebDAV")?;
            }
        }
        let mut entries = Vec::new();
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if (id == LEGACY_ID || Uuid::parse_str(&id).is_ok())
                && entry.file_type()?.is_dir()
                && entry.path().join("name.json").is_file()
            {
                entries.push(Arc::new(WebDavProfile::new(&root, id)));
            }
        }
        Ok(Self { root, entries: Mutex::new(entries), registration: Default::default() })
    }

    pub fn get(&self, id: &str) -> Result<Arc<WebDavProfile>, String> {
        self.entries.lock().map_err(|e| e.to_string())?.iter()
            .find(|profile| profile.id == id).cloned().ok_or_else(|| "找不到 WebDAV 保管庫".into())
    }

    pub fn has_target(&self, target: &str) -> Result<bool, String> {
        for profile in self.entries.lock().map_err(|e| e.to_string())?.iter() {
            let cfg = profile.sync.lock().map_err(|e| e.to_string())?.config_clone();
            if cfg.target_url().is_ok_and(|url| url == target) { return Ok(true); }
        }
        Ok(false)
    }

    pub fn draft(&self) -> Arc<WebDavProfile> {
        Arc::new(WebDavProfile::new(&self.root, Uuid::new_v4().to_string()))
    }

    pub fn add(&self, profile: Arc<WebDavProfile>) -> Result<(), String> {
        self.entries.lock().map_err(|e| e.to_string())?.push(profile);
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<WebDavProfileView>, String> {
        let entries = self.entries.lock().map_err(|e| e.to_string())?.clone();
        let mut views = entries.iter().map(|profile| profile.view()).collect::<Result<Vec<_>, _>>()?;
        views.sort_by(|a,b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        Ok(views)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{SaveSyncConfigInput, SyncMode};

    #[test]
    fn remote_profiles_reopen_with_separate_caches_and_targets() {
        let root = std::env::temp_dir().join(format!("mistrdp-profiles-{}",Uuid::new_v4()));
        let profiles = WebDavProfiles::new(&root).unwrap();
        let mut ids = Vec::new();
        for (name, password) in [("Home", "Home#1234"),("Work", "Work#1234")] {
            let profile = profiles.draft();
            profile.vault.lock().unwrap().create(password).unwrap();
            profile.vault.lock().unwrap().change_group(None,Some(name)).unwrap();
            profile.sync.lock().unwrap().save_config(SaveSyncConfigInput {
                mode:SyncMode::WebDav, webdav_url:"http://127.0.0.1/dav".into(), webdav_username:String::new(),
                webdav_file_name:format!("{name}.json"), webdav_allow_invalid_certs:false, webdav_password:None,
                clear_webdav_password:false,
            }).unwrap();
            profile.set_name(name).unwrap();
            ids.push(profile.id.clone());
            profiles.add(profile).unwrap();
        }
        drop(profiles);
        let reopened = WebDavProfiles::new(&root).unwrap();
        assert!(reopened.has_target("http://127.0.0.1/dav/Home.json").unwrap());
        assert!(reopened.has_target("http://127.0.0.1/dav/Work.json").unwrap());
        let home = reopened.get(&ids[0]).unwrap();
        let work = reopened.get(&ids[1]).unwrap();
        assert_ne!(home.vault.lock().unwrap().status().vault_path, work.vault.lock().unwrap().status().vault_path);
        assert!(work.vault.lock().unwrap().unlock("Home#1234").is_err());
        assert_eq!(home.vault.lock().unwrap().unlock("Home#1234").unwrap().groups[0].name,"Home");
        assert_eq!(work.vault.lock().unwrap().unlock("Work#1234").unwrap().groups[0].name,"Work");
        let _ = fs::remove_dir_all(root);
    }
}
