use std::path::Path;

use anyhow::{Context, Result};
use chromine_shared::fsutil;
use serde_json::{Map, Value, json};

use crate::config::Theme;

const THEME_FILE: &str = "chromine-default-theme";

fn set(root: &mut Value, path: &[&str], v: Value) {
    let mut cur = root;
    for key in &path[..path.len() - 1] {
        if !cur.get(*key).is_some_and(Value::is_object) {
            cur[*key] = Value::Object(Map::new());
        }
        cur = cur.get_mut(*key).expect("just inserted");
    }
    cur[path[path.len() - 1]] = v;
}

fn load(path: &Path) -> Value {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) if v.is_object() => v,
            _ => {
                tracing::warn!(file = %path.display(), "Chrome preference file is not valid JSON; recreating it");
                json!({})
            }
        },
        Err(_) => json!({}),
    }
}

fn store(path: &Path, v: &Value) -> Result<()> {
    let dir = path.parent().context("preference file has no parent")?;
    let name = path.file_name().and_then(|n| n.to_str()).context("preference file name")?;
    fsutil::atomic_write(dir, name, &serde_json::to_vec(v)?, 0o600)
        .with_context(|| format!("cannot write {}", path.display()))
}

pub fn apply(profile_dir: &Path, incoming: &Path, safe_browsing: bool, theme: Theme) -> Result<()> {
    let default = profile_dir.join("Default");
    if !default.exists() {
        std::fs::create_dir_all(&default).with_context(|| format!("cannot create {}", default.display()))?;
    }
    let dir = incoming.to_string_lossy().into_owned();

    let prefs_path = default.join("Preferences");
    let mut prefs = load(&prefs_path);
    set(&mut prefs, &["download", "default_directory"], json!(dir));
    set(&mut prefs, &["download", "prompt_for_download"], json!(false));
    set(&mut prefs, &["download", "directory_upgrade"], json!(true));
    set(&mut prefs, &["savefile", "default_directory"], json!(dir));
    set(&mut prefs, &["safebrowsing", "enabled"], json!(safe_browsing));
    set(&mut prefs, &["safebrowsing", "enhanced"], json!(false));
    let theme_file = profile_dir.join(THEME_FILE);
    let new_theme = std::fs::read_to_string(&theme_file).ok().as_deref().map(str::trim) != Some(theme.name());
    if new_theme {
        set(&mut prefs, &["browser", "theme", "color_scheme2"], json!(if theme == Theme::Dark { 2 } else { 1 }));
    }
    store(&prefs_path, &prefs)?;
    if new_theme {
        fsutil::atomic_write(profile_dir, THEME_FILE, theme.name().as_bytes(), 0o600)
            .with_context(|| format!("cannot write {}", theme_file.display()))?;
    }

    let state_path = profile_dir.join("Local State");
    let mut state = load(&state_path);
    set(&mut state, &["hardware_acceleration_mode", "enabled"], json!(false));
    set(&mut state, &["background_mode", "enabled"], json!(false));
    store(&state_path, &state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_settings_in_a_new_profile() {
        let d = tempfile::tempdir().unwrap();
        apply(d.path(), Path::new("/downloads/incoming"), false, Theme::Dark).unwrap();
        let p: Value = serde_json::from_slice(&std::fs::read(d.path().join("Default/Preferences")).unwrap()).unwrap();
        assert_eq!(p["download"]["default_directory"], "/downloads/incoming");
        assert_eq!(p["download"]["prompt_for_download"], false);
        assert_eq!(p["savefile"]["default_directory"], "/downloads/incoming");
        assert_eq!(p["safebrowsing"]["enabled"], false);
        assert_eq!(p["safebrowsing"]["enhanced"], false);
        apply(d.path(), Path::new("/downloads/incoming"), true, Theme::Dark).unwrap();
        let p: Value = serde_json::from_slice(&std::fs::read(d.path().join("Default/Preferences")).unwrap()).unwrap();
        assert_eq!(p["safebrowsing"]["enabled"], true);
        let s: Value = serde_json::from_slice(&std::fs::read(d.path().join("Local State")).unwrap()).unwrap();
        assert_eq!(s["hardware_acceleration_mode"]["enabled"], false);
        assert_eq!(s["background_mode"]["enabled"], false);
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(d.path().join("Default/Preferences")).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn keeps_existing_settings_and_resets_download_choices() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("Default")).unwrap();
        std::fs::write(
            d.path().join("Default/Preferences"),
            r#"{"browser":{"show_home_button":true},"download":{"default_directory":"/tmp/elsewhere","prompt_for_download":true,"extensions_to_open":"pdf"},"savefile":"odd"}"#,
        )
        .unwrap();
        std::fs::write(d.path().join("Local State"), r#"{"profile":{"info_cache":{"Default":{}}}}"#).unwrap();
        apply(d.path(), Path::new("/downloads/incoming"), false, Theme::Dark).unwrap();
        let p: Value = serde_json::from_slice(&std::fs::read(d.path().join("Default/Preferences")).unwrap()).unwrap();
        assert_eq!(p["browser"]["show_home_button"], true);
        assert_eq!(p["download"]["extensions_to_open"], "pdf");
        assert_eq!(p["download"]["default_directory"], "/downloads/incoming");
        assert_eq!(p["download"]["prompt_for_download"], false);
        assert_eq!(p["savefile"]["default_directory"], "/downloads/incoming");
        let s: Value = serde_json::from_slice(&std::fs::read(d.path().join("Local State")).unwrap()).unwrap();
        assert!(s["profile"]["info_cache"]["Default"].is_object());
        assert_eq!(s["background_mode"]["enabled"], false);
    }

    #[test]
    fn replaces_corrupt_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("Default")).unwrap();
        std::fs::write(d.path().join("Default/Preferences"), b"{not json").unwrap();
        apply(d.path(), Path::new("/downloads/incoming"), false, Theme::Dark).unwrap();
        let p: Value = serde_json::from_slice(&std::fs::read(d.path().join("Default/Preferences")).unwrap()).unwrap();
        assert_eq!(p["download"]["prompt_for_download"], false);
    }

    #[test]
    fn the_default_theme_is_set_once_so_a_choice_made_in_chrome_sticks() {
        let d = tempfile::tempdir().unwrap();
        let prefs = d.path().join("Default/Preferences");
        let scheme = || -> Value {
            serde_json::from_slice::<Value>(&std::fs::read(&prefs).unwrap()).unwrap()["browser"]["theme"]["color_scheme2"].clone()
        };
        apply(d.path(), Path::new("/downloads/incoming"), false, Theme::Dark).unwrap();
        assert_eq!(scheme(), 2);
        std::fs::write(&prefs, r#"{"browser":{"theme":{"color_scheme2":1}}}"#).unwrap();
        apply(d.path(), Path::new("/downloads/incoming"), false, Theme::Dark).unwrap();
        assert_eq!(scheme(), 1);
        apply(d.path(), Path::new("/downloads/incoming"), false, Theme::Light).unwrap();
        assert_eq!(scheme(), 1);
        std::fs::write(&prefs, r#"{"browser":{"theme":{"color_scheme2":2}}}"#).unwrap();
        apply(d.path(), Path::new("/downloads/incoming"), false, Theme::Light).unwrap();
        assert_eq!(scheme(), 2);
        apply(d.path(), Path::new("/downloads/incoming"), false, Theme::Dark).unwrap();
        assert_eq!(scheme(), 2);
        std::fs::write(&prefs, r#"{"browser":{"theme":{"color_scheme2":2}}}"#).unwrap();
        apply(d.path(), Path::new("/downloads/incoming"), false, Theme::Light).unwrap();
        assert_eq!(scheme(), 1);
    }
}
