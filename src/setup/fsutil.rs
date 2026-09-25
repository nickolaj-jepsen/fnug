//! File helpers for the files setup edits.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Replace `path` with `contents` in one step, so a crash leaves the old file or the new one,
/// never a partial one. A symlink is followed and its target replaced. The file gets `mode` if
/// given, otherwise the old file's mode, otherwise the default for new files. Missing parent
/// directories are created.
///
/// # Errors
///
/// Returns an IO error if the file can't be written or renamed into place; no temporary file is
/// left behind.
pub fn write_atomic(path: &Path, contents: &str, mode: Option<u32>) -> io::Result<()> {
    let path = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a file path: {}", path.display()),
        ));
    };
    fs::create_dir_all(dir)?;
    let mode = mode.or_else(|| {
        fs::metadata(&path)
            .ok()
            .map(|meta| meta.permissions().mode() & 0o7777)
    });

    let (tmp, mut file) = create_temp(dir, &name.to_string_lossy())?;
    let written = file
        .write_all(contents.as_bytes())
        .and_then(|()| match mode {
            Some(mode) => file.set_permissions(fs::Permissions::from_mode(mode)),
            None => Ok(()),
        })
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&tmp, &path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// The first executable file named `name` in the directories on `PATH`.
#[must_use]
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    find_in(name, &std::env::var_os("PATH")?)
}

fn find_in(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| {
            fs::metadata(candidate)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

/// A new file next to the target, named so it is recognisable if something goes wrong.
fn create_temp(dir: &Path, name: &str) -> io::Result<(PathBuf, fs::File)> {
    let mut attempt = 0;
    loop {
        let tmp = dir.join(format!(".{name}.fnug-{}-{attempt}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && attempt < 100 => attempt += 1,
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn creates_parents_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/file");
        write_atomic(&path, "one", None).unwrap();
        write_atomic(&path, "two", None).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["file"]);
    }

    #[test]
    fn keeps_the_old_mode_unless_given_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hook");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();

        write_atomic(&path, "new", None).unwrap();
        assert_eq!(mode(&path), 0o750);
        write_atomic(&path, "newer", Some(0o755)).unwrap();
        assert_eq!(mode(&path), 0o755);
    }

    #[test]
    fn find_in_skips_missing_and_non_executable_files() {
        let dir = tempfile::tempdir().unwrap();
        let (plain, exec) = (dir.path().join("plain"), dir.path().join("exec"));
        fs::create_dir(&plain).unwrap();
        fs::create_dir(&exec).unwrap();
        fs::write(plain.join("tool"), "").unwrap();
        fs::write(exec.join("tool"), "").unwrap();
        fs::set_permissions(exec.join("tool"), fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths([dir.path().join("missing"), plain, exec.clone()]).unwrap();

        assert_eq!(find_in("tool", &path), Some(exec.join("tool")));
        assert_eq!(find_in("other", &path), None);
    }

    #[test]
    fn writes_through_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        fs::write(&target, "old").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        write_atomic(&link, "new", None).unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
    }
}
