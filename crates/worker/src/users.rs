use std::collections::HashMap;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chromine_shared::{SCHEMA_VERSION, USERS_FILE, UsersFile, fsutil, now_ms};
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::db::Db;
use crate::ingest::Ingest;
use crate::settings::Registry;

pub const MIN_PASSWORD: usize = 8;
const SHARED_DIR_MODE: u32 = 0o2770;

pub fn check_username(raw: &str, admin: &str) -> std::result::Result<String, String> {
    let name = raw.trim().to_ascii_lowercase();
    if !(3..=32).contains(&name.len()) {
        return Err("Usernames are 3 to 32 characters long.".into());
    }
    let allowed = name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
    if !allowed || !name.as_bytes()[0].is_ascii_alphanumeric() {
        return Err("Use letters, numbers, dots, dashes and underscores, starting with a letter or number.".into());
    }
    if name.eq_ignore_ascii_case(admin.trim()) {
        return Err("That username belongs to the admin.".into());
    }
    Ok(name)
}

pub fn check_user_password(p: &str) -> std::result::Result<(), String> {
    let n = p.chars().count();
    if n < MIN_PASSWORD {
        return Err(format!("Passwords need at least {MIN_PASSWORD} characters."));
    }
    if n > 256 {
        return Err("Passwords can be at most 256 characters.".into());
    }
    if p.chars().any(char::is_control) {
        return Err("Passwords cannot contain control characters.".into());
    }
    Ok(())
}

pub fn new_user_id() -> String {
    let mut b = [0u8; 16];
    for x in b.iter_mut() {
        *x = rand::random();
    }
    hex::encode(b)
}

fn shared_dir(dir: &Path) -> std::io::Result<()> {
    if !dir.exists() {
        std::fs::DirBuilder::new().mode(SHARED_DIR_MODE).create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(SHARED_DIR_MODE))?;
    }
    Ok(())
}

fn prepare_space(cfg: &Config, owner: &str) -> std::io::Result<()> {
    let space = cfg.space_dir(owner);
    if let Some(parent) = space.parent() {
        shared_dir(parent)?;
    }
    shared_dir(&space)?;
    shared_dir(&cfg.incoming_dir_of(owner))?;
    if let Some(events) = cfg.completed_dir_of(owner).parent() {
        shared_dir(events)?;
    }
    shared_dir(&cfg.completed_dir_of(owner))
}

pub struct Spaces {
    cfg: Arc<Config>,
    template: Ingest,
    settings: Arc<Registry>,
    stop: CancellationToken,
    running: Mutex<HashMap<String, (Ingest, CancellationToken)>>,
    writing: tokio::sync::Mutex<()>,
}

impl Spaces {
    pub fn new(cfg: Arc<Config>, template: Ingest, settings: Arc<Registry>) -> Spaces {
        Spaces {
            cfg,
            template,
            settings,
            stop: CancellationToken::new(),
            running: Mutex::new(HashMap::new()),
            writing: tokio::sync::Mutex::new(()),
        }
    }

    pub fn ingest(&self, owner: &str) -> Option<Ingest> {
        if owner.is_empty() {
            return Some(self.template.clone());
        }
        self.running.lock().unwrap().get(owner).map(|e| e.0.clone())
    }

    pub fn shutdown(&self) {
        self.stop.cancel();
    }

    fn start(&self, owner: &str) {
        if self.running.lock().unwrap().contains_key(owner) {
            return;
        }
        if let Err(e) = prepare_space(&self.cfg, owner) {
            tracing::warn!(user = %owner, error = %e, "cannot create a user's download folders");
        }
        let ingest = Ingest {
            owner: owner.to_string(),
            incoming: self.cfg.incoming_dir_of(owner),
            completed: self.cfg.completed_dir_of(owner),
            settings: self.settings.of(owner).subscribe(),
            ..self.template.clone()
        };
        let token = self.stop.child_token();
        let run = ingest.clone();
        let sd = token.clone();
        let id = owner.to_string();
        tokio::spawn(async move {
            if let Err(e) = run.run(sd).await {
                tracing::error!(user = %id, error = %e, "download ingestion for a user stopped");
            }
        });
        self.running.lock().unwrap().insert(owner.to_string(), (ingest, token));
    }

    pub async fn sync(&self, db: &Db) -> Result<()> {
        let _w = self.writing.lock().await;
        let rows = db.user_ids(now_ms()).await?;
        let known: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
        let file = UsersFile {
            schema: SCHEMA_VERSION,
            known: known.clone(),
            active: rows.iter().filter(|r| r.2).map(|r| r.0.clone()).collect(),
            names: rows.iter().map(|r| (r.0.clone(), r.1.clone())).collect(),
        };
        let bytes = serde_json::to_vec(&file)?;
        let roots: Vec<_> = std::iter::once(self.cfg.vnc_root())
            .chain(self.cfg.code_socket.as_ref().and_then(|s| s.parent()).map(Path::to_path_buf))
            .collect();
        tokio::task::spawn_blocking(move || {
            roots.iter().try_for_each(|root| fsutil::atomic_write(root, USERS_FILE, &bytes, 0o640))
        })
        .await?
        .context("cannot publish the user list for the browser")?;
        for id in &known {
            self.settings.of(id);
            self.start(id);
        }
        let gone: Vec<(String, CancellationToken)> = {
            let mut running = self.running.lock().unwrap();
            let ids: Vec<String> = running.keys().filter(|id| !known.contains(id)).cloned().collect();
            ids.into_iter().filter_map(|id| running.remove(&id).map(|(_, t)| (id, t))).collect()
        };
        for (_, token) in gone {
            token.cancel();
        }
        Ok(())
    }

    pub async fn sync_logged(&self, db: &Db) {
        if let Err(e) = self.sync(db).await {
            tracing::error!(error = format!("{e:#}"), "cannot update which user browsers should run");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_are_normalized_and_checked() {
        assert_eq!(check_username(" MyUser01 ", "admin").unwrap(), "myuser01");
        assert_eq!(check_username("a.b-c_d", "admin").unwrap(), "a.b-c_d");
        assert!(check_username("ab", "admin").is_err());
        assert!(check_username(&"a".repeat(33), "admin").is_err());
        assert!(check_username("-lead", "admin").is_err());
        assert!(check_username("has space", "admin").is_err());
        assert!(check_username("../etc", "admin").is_err());
        assert!(check_username("ADMIN", "admin").is_err());
    }

    #[test]
    fn user_passwords_need_eight_characters() {
        assert!(check_user_password("pass@1234").is_ok());
        assert!(check_user_password("short1!").is_err());
        assert!(check_user_password(&"x".repeat(257)).is_err());
        assert!(check_user_password("bad\npassword").is_err());
    }

    #[test]
    fn user_ids_are_random_hex() {
        let a = new_user_id();
        assert!(chromine_shared::is_valid_user_id(&a));
        assert_ne!(a, new_user_id());
    }
}
