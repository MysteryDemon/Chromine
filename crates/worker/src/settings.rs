use std::collections::HashMap;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use chromine_shared::fsutil;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::{Mutex, watch};

use crate::config::{Config, StreamMode, WebrtcCodec};

const FILE: &str = "settings.json";
const USER_DIR: &str = "user-settings";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoQuality {
    Low,
    Medium,
    High,
    Max,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Live {
    pub filecloud: bool,
    pub keep_local: bool,
    pub stream_mode: StreamMode,
    pub display_scale: f64,
    pub webrtc_codec: WebrtcCodec,
    pub webrtc_fps: u32,
    pub webrtc_quality: VideoQuality,
    pub webrtc_host: String,
    pub vnc_quality: u8,
    pub vnc_compression: u8,
}

impl Live {
    pub fn from_config(cfg: &Config) -> Live {
        Live {
            filecloud: true,
            keep_local: !cfg.delete_after_success,
            stream_mode: cfg.stream_mode,
            display_scale: cfg.display_scale,
            webrtc_codec: cfg.webrtc_codec,
            webrtc_fps: 30,
            webrtc_quality: VideoQuality::Medium,
            webrtc_host: cfg.webrtc_host.clone().unwrap_or_default(),
            vnc_quality: 9,
            vnc_compression: 2,
        }
    }

    pub fn webrtc_host(&self) -> Option<&str> {
        Some(self.webrtc_host.as_str()).filter(|h| !h.is_empty())
    }

    fn checked(mut self) -> Result<Live> {
        self.webrtc_host = self.webrtc_host.trim().to_string();
        if !(1.0..=3.0).contains(&self.display_scale) {
            bail!("Display scale must be between 1 and 3");
        }
        if !(5..=60).contains(&self.webrtc_fps) {
            bail!("Frame rate must be between 5 and 60");
        }
        if self.vnc_quality > 9 || self.vnc_compression > 9 {
            bail!("VNC quality and compression go from 0 to 9");
        }
        if self.webrtc_host.len() > 253
            || !self.webrtc_host.chars().all(|c| c.is_ascii_alphanumeric() || ".-_:[]".contains(c))
        {
            bail!("WebRTC host must be a host name or IP address");
        }
        Ok(self)
    }
}

pub struct Store {
    defaults: Live,
    file: PathBuf,
    scale_file: PathBuf,
    tx: watch::Sender<Live>,
    writing: Mutex<()>,
}

impl Store {
    pub fn open(cfg: &Config) -> Store {
        Store::open_at(
            Live::from_config(cfg),
            cfg.state_dir.join(FILE),
            cfg.vnc_socket.with_file_name(chromine_shared::DISPLAY_SCALE_FILE),
        )
    }

    fn open_at(defaults: Live, file: PathBuf, scale_file: PathBuf) -> Store {
        let live = match std::fs::read(&file) {
            Err(_) => defaults.clone(),
            Ok(bytes) => serde_json::from_slice::<Map<String, Value>>(&bytes)
                .map_err(anyhow::Error::from)
                .and_then(|saved| merge(&defaults, saved, false))
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, file = %file.display(), "ignoring saved settings");
                    defaults.clone()
                }),
        };
        let store = Store { defaults, file, scale_file, tx: watch::channel(live).0, writing: Mutex::new(()) };
        store.share_scale();
        store
    }

    pub fn current(&self) -> Live {
        self.tx.borrow().clone()
    }

    pub fn defaults(&self) -> &Live {
        &self.defaults
    }

    pub fn subscribe(&self) -> watch::Receiver<Live> {
        self.tx.subscribe()
    }

    pub async fn update(&self, patch: Map<String, Value>) -> Result<Live> {
        let _w = self.writing.lock().await;
        let next = merge(&self.current(), patch, true)?;
        self.save(next)
    }

    pub async fn reset(&self) -> Result<Live> {
        let _w = self.writing.lock().await;
        self.save(self.defaults.clone())
    }

    fn save(&self, next: Live) -> Result<Live> {
        let dir = self.file.parent().context("settings directory")?;
        let name = self.file.file_name().and_then(|n| n.to_str()).context("settings file name")?;
        let saved = serde_json::to_vec_pretty(&changes(&next, &self.defaults))?;
        fsutil::atomic_write(dir, name, &saved, 0o600).context("cannot save settings")?;
        let rescaled = next.display_scale != self.tx.borrow().display_scale;
        self.tx.send_replace(next.clone());
        if rescaled {
            self.share_scale();
        }
        Ok(next)
    }

    fn share_scale(&self) {
        let scale = self.tx.borrow().display_scale.to_string();
        let (Some(dir), Some(name)) = (self.scale_file.parent(), self.scale_file.file_name().and_then(|n| n.to_str()))
        else {
            return;
        };
        if let Err(e) = fsutil::atomic_write(dir, name, scale.as_bytes(), 0o640) {
            tracing::warn!(error = %e, file = %self.scale_file.display(), "cannot pass the display scale to the browser");
        }
    }
}

fn shared_dir(dir: &Path) -> std::io::Result<()> {
    if !dir.exists() {
        std::fs::DirBuilder::new().mode(0o2770).create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o2770))?;
    }
    Ok(())
}

pub struct Registry {
    cfg: Arc<Config>,
    admin: Arc<Store>,
    users: std::sync::Mutex<HashMap<String, Arc<Store>>>,
}

impl Registry {
    pub fn open(cfg: Arc<Config>) -> Registry {
        let admin = Arc::new(Store::open(&cfg));
        Registry { cfg, admin, users: std::sync::Mutex::new(HashMap::new()) }
    }

    fn user_file(&self, owner: &str) -> PathBuf {
        self.cfg.state_dir.join(USER_DIR).join(format!("{owner}.json"))
    }

    pub fn of(&self, owner: &str) -> Arc<Store> {
        if owner.is_empty() {
            return self.admin.clone();
        }
        let mut users = self.users.lock().unwrap();
        if let Some(store) = users.get(owner) {
            return store.clone();
        }
        let _ = std::fs::DirBuilder::new().mode(0o700).create(self.cfg.state_dir.join(USER_DIR));
        let sockets = chromine_shared::user_dir(&self.cfg.vnc_root(), owner);
        if let Some(parent) = sockets.parent()
            && let Err(e) = shared_dir(parent).and_then(|_| shared_dir(&sockets))
        {
            tracing::warn!(user = %owner, error = %e, "cannot create a user's display folder");
        }
        let store = Arc::new(Store::open_at(
            Live::from_config(&self.cfg),
            self.user_file(owner),
            sockets.join(chromine_shared::DISPLAY_SCALE_FILE),
        ));
        users.insert(owner.to_string(), store.clone());
        store
    }

    pub fn forget(&self, owner: &str) {
        self.users.lock().unwrap().remove(owner);
        let _ = std::fs::remove_file(self.user_file(owner));
    }
}

fn merge(base: &Live, patch: Map<String, Value>, strict: bool) -> Result<Live> {
    let Value::Object(mut fields) = serde_json::to_value(base)? else { bail!("settings are not an object") };
    for (k, v) in patch {
        match fields.get_mut(&k) {
            Some(slot) => *slot = v,
            None if strict => bail!("unknown setting {k:?}"),
            None => {}
        }
    }
    serde_json::from_value::<Live>(Value::Object(fields)).map_err(|e| anyhow!("invalid setting: {e}"))?.checked()
}

fn changes(live: &Live, defaults: &Live) -> Map<String, Value> {
    let (Ok(Value::Object(a)), Ok(Value::Object(b))) = (serde_json::to_value(live), serde_json::to_value(defaults))
    else {
        return Map::new();
    };
    a.into_iter().filter(|(k, v)| b.get(k) != Some(v)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn defaults() -> Live {
        Live {
            filecloud: true,
            keep_local: false,
            stream_mode: StreamMode::Vnc,
            display_scale: 1.0,
            webrtc_codec: WebrtcCodec::Auto,
            webrtc_fps: 30,
            webrtc_quality: VideoQuality::Medium,
            webrtc_host: String::new(),
            vnc_quality: 9,
            vnc_compression: 2,
        }
    }

    fn patch(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn a_patch_changes_only_the_named_settings() {
        let d = defaults();
        let l = merge(
            &d,
            patch(json!({"filecloud": false, "stream_mode": "webrtc", "display_scale": 1.5, "webrtc_host": " 203.0.113.5 "})),
            true,
        )
        .unwrap();
        assert!(!l.filecloud);
        assert_eq!(l.stream_mode, StreamMode::Webrtc);
        assert_eq!(l.display_scale, 1.5);
        assert_eq!(l.webrtc_host(), Some("203.0.113.5"));
        assert_eq!(l.webrtc_fps, 30);
        let saved = changes(&l, &d);
        let mut keys: Vec<&str> = saved.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["display_scale", "filecloud", "stream_mode", "webrtc_host"]);
        assert_eq!(merge(&d, saved, false).unwrap(), l);
        for codec in ["auto", "av1", "h265", "h264", "vp9", "vp8"] {
            assert!(merge(&d, patch(json!({ "webrtc_codec": codec })), true).is_ok(), "{codec}");
        }
    }

    #[test]
    fn bad_values_are_refused() {
        let d = defaults();
        for bad in [
            json!({"display_scale": 4}),
            json!({"display_scale": 0.5}),
            json!({"webrtc_fps": 0}),
            json!({"webrtc_fps": 120}),
            json!({"vnc_quality": 10}),
            json!({"vnc_compression": -1}),
            json!({"webrtc_codec": "theora"}),
            json!({"filecloud": "yes"}),
            json!({"webrtc_host": "evil.example/path"}),
            json!({"webrtc_host": "a b"}),
            json!({"stream_mode": null}),
            json!({"password": "x"}),
        ] {
            assert!(merge(&d, patch(bad.clone()), true).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn each_user_keeps_separate_settings() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        let vnc = d.path().join("vnc");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&vnc).unwrap();
        let cfg =
            Config { state_dir: state.clone(), vnc_socket: vnc.join("vnc.sock"), ..crate::config::Config::for_tests() };
        let reg = Registry::open(Arc::new(cfg));
        let id = "0123456789abcdef0123456789abcdef";
        reg.of(id).update(patch(json!({"display_scale": 2, "filecloud": false}))).await.unwrap();
        assert_eq!(reg.of("").current().display_scale, 1.0);
        assert!(reg.of("").current().filecloud);
        assert_eq!(std::fs::read_to_string(vnc.join("users").join(id).join("display-scale")).unwrap(), "2");
        assert!(
            !vnc.join("display-scale").exists() || std::fs::read_to_string(vnc.join("display-scale")).unwrap() == "1"
        );
        let reopened = Registry::open(reg.cfg.clone());
        assert_eq!(reopened.of(id).current().display_scale, 2.0);
        assert!(!reopened.of(id).current().filecloud);
        reopened.forget(id);
        assert!(!state.join(USER_DIR).join(format!("{id}.json")).exists());
    }

    #[test]
    fn saved_settings_ignore_names_they_do_not_know() {
        let l = merge(&defaults(), patch(json!({"retired": 1, "webrtc_fps": 60})), false).unwrap();
        assert_eq!(l.webrtc_fps, 60);
    }
}
