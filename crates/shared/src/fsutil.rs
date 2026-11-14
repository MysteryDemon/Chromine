use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};

use crate::DiskSpace;

pub fn atomic_write(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> io::Result<()> {
    if name.contains('/') || name.starts_with('.') || name.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid record name"));
    }
    let tmp_name = format!(".tmp-{}-{}-{}", std::process::id(), crate::now_ms(), name);
    let tmp = dir.join(&tmp_name);
    let result = (|| {
        let mut f =
            OpenOptions::new().write(true).create_new(true).mode(mode).custom_flags(libc_o_nofollow()).open(&tmp)?;
        f.set_permissions(fs::Permissions::from_mode(mode))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, dir.join(name))?;
        File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub fn open_regular_nofollow(path: &Path) -> io::Result<(File, Metadata)> {
    let f = OpenOptions::new().read(true).custom_flags(libc_o_nofollow()).open(path)?;
    let meta = f.metadata()?;
    if !meta.file_type().is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
    }
    Ok((f, meta))
}

pub fn regular_file_lstat(path: &Path) -> io::Result<Metadata> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "refusing to follow a symlink"));
    }
    if !meta.file_type().is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
    }
    Ok(meta)
}

pub fn is_direct_child(dir: &Path, path: &Path) -> bool {
    if !path.is_absolute() || !dir.is_absolute() {
        return false;
    }
    let Some(parent) = path.parent() else { return false };
    if parent != dir {
        return false;
    }
    if !path.components().all(|c| matches!(c, Component::RootDir | Component::Normal(_))) {
        return false;
    }
    match path.file_name().and_then(|n| n.to_str()) {
        Some(name) => !name.is_empty() && name != "." && name != "..",
        None => false,
    }
}

pub fn disk_space(path: &Path) -> io::Result<DiskSpace> {
    let st = rustix::fs::statvfs(path).map_err(io::Error::from)?;
    let frsize = st.f_frsize;
    Ok(DiskSpace {
        available_bytes: st.f_bavail.saturating_mul(frsize),
        total_bytes: st.f_blocks.saturating_mul(frsize),
    })
}

pub fn identity(meta: &Metadata) -> (u64, u64) {
    (meta.dev(), meta.ino())
}

const fn libc_o_nofollow() -> i32 {
    libc::O_NOFOLLOW | libc::O_CLOEXEC
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn direct_child_checks() {
        let d = Path::new("/downloads/incoming");
        assert!(is_direct_child(d, Path::new("/downloads/incoming/a.zip")));
        assert!(is_direct_child(d, Path::new("/downloads/incoming/report (1).zip")));
        assert!(!is_direct_child(d, Path::new("/downloads/incoming/../state/db")));
        assert!(!is_direct_child(d, Path::new("/downloads/incoming/sub/a.zip")));
        assert!(!is_direct_child(d, Path::new("/downloads/other/a.zip")));
        assert!(!is_direct_child(d, Path::new("relative/a.zip")));
        assert!(!is_direct_child(d, Path::new("/downloads/incoming")));
        assert!(!is_direct_child(d, Path::new("/downloads/incoming/.")));
    }

    #[test]
    fn atomic_write_and_nofollow() {
        let tmp = tempfile::tempdir().unwrap();
        atomic_write(tmp.path(), "a.json", b"{}", 0o640).unwrap();
        assert_eq!(fs::read(tmp.path().join("a.json")).unwrap(), b"{}");
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);

        assert!(atomic_write(tmp.path(), "../x", b"", 0o600).is_err());
        assert!(atomic_write(tmp.path(), ".hidden", b"", 0o600).is_err());

        symlink(tmp.path().join("a.json"), tmp.path().join("link")).unwrap();
        assert!(open_regular_nofollow(&tmp.path().join("link")).is_err());
        assert!(regular_file_lstat(&tmp.path().join("link")).is_err());
        assert!(open_regular_nofollow(&tmp.path().join("a.json")).is_ok());
        assert!(open_regular_nofollow(tmp.path()).is_err());
    }

    #[test]
    fn atomic_write_sets_the_exact_mode_despite_the_umask() {
        let tmp = tempfile::tempdir().unwrap();
        let old = unsafe { libc::umask(0o077) };
        let r = atomic_write(tmp.path(), "shared.json", b"{}", 0o640);
        unsafe { libc::umask(old) };
        r.unwrap();
        let mode = fs::metadata(tmp.path().join("shared.json")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
    }

    #[test]
    fn disk_space_reports_something() {
        let d = disk_space(Path::new("/")).unwrap();
        assert!(d.total_bytes > 0);
    }
}
