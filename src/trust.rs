//! Which config files fnug may load without being told to, like git's `safe.directory`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Directories whose configs are trusted whoever owns them, separated like `PATH`; `*` trusts
/// every directory.
pub const SAFE_DIRECTORIES_ENV: &str = "FNUG_SAFE_DIRECTORIES";

/// Decides whether a config file that fnug found by itself (by searching upward, as a parent
/// workspace root, or as a workspace package) may be loaded.
///
/// A config is trusted if both the file and, for a symlink, its target are owned by `uid` or
/// root, or if it is under one of `safe_dirs`. On non-unix platforms every config is trusted.
#[derive(Debug, Clone, Default)]
pub struct TrustPolicy {
    /// The user configs must belong to; `None` means this process's effective user.
    pub uid: Option<u32>,
    /// Configs under these directories are trusted whoever owns them.
    pub safe_dirs: Vec<PathBuf>,
    /// Trust every config.
    pub trust_all: bool,
}

/// A config file owned by another user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Untrusted {
    pub path: PathBuf,
    /// The file's owner.
    pub owner: u32,
    /// The user it should belong to.
    pub uid: u32,
}

impl TrustPolicy {
    /// The policy for this process, with safe directories from `FNUG_SAFE_DIRECTORIES`.
    #[must_use]
    pub fn from_env() -> Self {
        std::env::var_os(SAFE_DIRECTORIES_ENV)
            .map(|value| Self::with_safe_directories(&value))
            .unwrap_or_default()
    }

    /// The policy for a `FNUG_SAFE_DIRECTORIES` value. Relative entries resolve against the
    /// working directory.
    #[must_use]
    pub fn with_safe_directories(value: &OsStr) -> Self {
        let mut policy = TrustPolicy::default();
        for dir in std::env::split_paths(value) {
            if dir.as_os_str() == "*" {
                policy.trust_all = true;
            } else if !dir.as_os_str().is_empty() {
                policy
                    .safe_dirs
                    .push(std::path::absolute(&dir).unwrap_or(dir));
            }
        }
        policy
    }

    /// Check whether the config file at `path` may be loaded.
    ///
    /// # Errors
    ///
    /// Returns `Untrusted` if the file, or the file it links to, is owned by someone other than
    /// the expected user or root and is not under a safe directory.
    pub fn check(&self, path: &Path) -> Result<(), Untrusted> {
        if self.trust_all || self.is_safe(path) {
            return Ok(());
        }
        self.check_owner(path)
    }

    fn is_safe(&self, path: &Path) -> bool {
        self.safe_dirs.iter().any(|dir| {
            path.starts_with(dir) || dir.canonicalize().is_ok_and(|dir| path.starts_with(dir))
        })
    }

    #[cfg(unix)]
    fn check_owner(&self, path: &Path) -> Result<(), Untrusted> {
        use std::os::unix::fs::MetadataExt;

        // SAFETY: geteuid has no preconditions and cannot fail.
        let uid = self.uid.unwrap_or_else(|| unsafe { libc::geteuid() });
        // lstat and stat: neither a foreign link to our file nor our link to a foreign file.
        for metadata in [std::fs::symlink_metadata(path), std::fs::metadata(path)] {
            // A file we can't stat fails to load anyway.
            let Ok(metadata) = metadata else { continue };
            let owner = metadata.uid();
            if owner != uid && owner != 0 {
                return Err(Untrusted {
                    path: path.to_path_buf(),
                    owner,
                    uid,
                });
            }
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn check_owner(&self, _path: &Path) -> Result<(), Untrusted> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_directories_parse() {
        let policy = TrustPolicy::with_safe_directories(OsStr::new("/a::/b"));
        assert_eq!(policy.safe_dirs, [PathBuf::from("/a"), PathBuf::from("/b")]);
        assert!(!policy.trust_all);
        assert!(TrustPolicy::with_safe_directories(OsStr::new("/a:*")).trust_all);
        assert!(
            TrustPolicy::with_safe_directories(OsStr::new(""))
                .safe_dirs
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn own_files_trusted_foreign_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".fnug.yaml");
        std::fs::write(&file, "").unwrap();
        assert_eq!(TrustPolicy::default().check(&file), Ok(()));
        if std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(&file).unwrap()) == 0 {
            return; // root's files are always trusted
        }

        let stranger = TrustPolicy {
            uid: Some(u32::MAX - 1),
            ..TrustPolicy::default()
        };
        let err = stranger.check(&file).unwrap_err();
        assert_eq!(err.uid, u32::MAX - 1);
        assert_eq!(err.path, file);

        let safe = TrustPolicy {
            safe_dirs: vec![dir.path().to_path_buf()],
            ..stranger.clone()
        };
        assert_eq!(safe.check(&file), Ok(()));
        let all = TrustPolicy {
            trust_all: true,
            ..stranger
        };
        assert_eq!(all.check(&file), Ok(()));
    }
}
