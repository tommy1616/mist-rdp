use crate::vault::write_atomic_text;
use anyhow::{Context, Result, anyhow, ensure};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::{fs, io::ErrorKind, path::PathBuf};

#[derive(Clone, Default, Deserialize, Serialize)]
struct Settings {
    password_hash: Option<String>,
    idle_minutes: u16,
}

#[derive(Serialize)]
pub struct AppSecurityStatus {
    configured: bool,
    pub locked: bool,
    idle_minutes: u16,
}

pub struct AppSecurity {
    path: PathBuf,
    settings: Settings,
    locked: bool,
}

impl AppSecurity {
    pub fn load(path: PathBuf) -> Result<Self> {
        let settings: Settings = match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).context("無法讀取 App 鎖定設定")?,
            Err(err) if err.kind() == ErrorKind::NotFound => Settings::default(),
            Err(err) => return Err(err).context("無法讀取 App 鎖定設定"),
        };
        Ok(Self { locked: settings.password_hash.is_some(), settings, path })
    }

    pub fn status(&self) -> AppSecurityStatus {
        AppSecurityStatus { configured: self.settings.password_hash.is_some(), locked: self.locked, idle_minutes: self.settings.idle_minutes }
    }

    fn verify(&self, password: &str) -> Result<()> {
        let stored = self.settings.password_hash.as_deref().ok_or_else(|| anyhow!("請先設定 App 主密碼"))?;
        let hash = PasswordHash::new(stored).map_err(|_| anyhow!("App 主密碼設定已損毀"))?;
        Argon2::default().verify_password(password.as_bytes(), &hash)
            .map_err(|_| anyhow!("App 主密碼錯誤"))
    }

    pub fn change_password(&mut self, current: &str, next: &str) -> Result<AppSecurityStatus> {
        if self.settings.password_hash.is_some() { self.verify(current)?; }
        ensure!(next.chars().count() >= 4, "App 主密碼至少 4 碼");
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default().hash_password(next.as_bytes(), &salt)
            .map_err(|error| anyhow!("無法設定 App 主密碼：{error}"))?.to_string();
        self.save(Settings { password_hash: Some(hash), ..self.settings.clone() })
    }

    pub fn set_idle(&mut self, minutes: u16) -> Result<AppSecurityStatus> {
        ensure!(minutes <= 1440, "自動鎖定時間最多 1440 分鐘");
        ensure!(minutes == 0 || self.settings.password_hash.is_some(), "請先設定 App 主密碼");
        self.save(Settings { idle_minutes: minutes, ..self.settings.clone() })
    }

    fn save(&mut self, settings: Settings) -> Result<AppSecurityStatus> {
        write_atomic_text(&self.path, &serde_json::to_string(&settings)?)?;
        self.settings = settings;
        Ok(self.status())
    }

    pub fn lock(&mut self) -> Result<AppSecurityStatus> {
        ensure!(self.settings.password_hash.is_some(), "請先設定 App 主密碼");
        self.locked = true;
        Ok(self.status())
    }

    pub fn unlock(&mut self, password: &str) -> Result<AppSecurityStatus> {
        self.verify(password)?;
        self.locked = false;
        Ok(self.status())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_password_and_timeout_survive_restart_without_plaintext() {
        let path = std::env::temp_dir().join(format!("mistrdp-app-lock-{}.json",uuid::Uuid::new_v4()));
        let mut app = AppSecurity::load(path.clone()).unwrap();
        assert!(!app.status().locked);
        assert!(!app.status().configured);
        assert!(app.set_idle(5).is_err());
        assert!(app.lock().is_err());
        app.change_password("", "App-only#1").unwrap();
        app.set_idle(5).unwrap();
        assert!(app.change_password("wrong", "App-only#2").is_err());
        app.change_password("App-only#1", "App-only#2").unwrap();
        assert!(!fs::read_to_string(&path).unwrap().contains("App-only"));

        let mut app = AppSecurity::load(path.clone()).unwrap();
        assert_eq!(app.status().idle_minutes, 5);
        assert!(app.status().locked);
        assert!(app.unlock("App-only#1").is_err());
        assert!(app.status().locked);
        assert!(!app.unlock("App-only#2").unwrap().locked);
        assert!(app.lock().unwrap().locked);
        fs::remove_file(path).unwrap();
    }
}
