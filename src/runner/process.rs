//! Starting a command's shell, and making sure it doesn't outlive its run.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Child;

use log::warn;

use crate::commands::command::Command;
use crate::process::ProcessHandle;

/// The environment variable that lists the files a command was selected for.
pub const FILES_VAR: &str = "FNUG_FILES";

/// The placeholder in a command's `cmd` for the files it was selected for.
const FILES_PLACEHOLDER: &[u8] = b"{files}";

/// The longest file list passed to a command, in bytes. Linux limits each argument and
/// environment string to 128 KiB.
const FILES_LIMIT: usize = 100 * 1024;

/// How to start a command: `program` with `args` in `cwd`, with `env` added to fnug's own
/// environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellInvocation {
    pub program: &'static str,
    pub args: [OsString; 2],
    pub cwd: PathBuf,
    /// Sorted by name.
    pub env: Vec<(OsString, OsString)>,
}

impl ShellInvocation {
    /// A process builder for the invocation. A [`FILES_VAR`] fnug itself was given is not
    /// passed on.
    #[must_use]
    pub fn command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(self.program);
        command
            .args(&self.args)
            .current_dir(&self.cwd)
            .env_remove(FILES_VAR)
            .envs(self.env.iter().map(|(k, v)| (k, v)));
        command
    }
}

/// Run `cmd.cmd` with `sh -c` in the command's cwd, or in `fallback_cwd` when it has none.
///
/// `files` are the changed files that selected the command, as absolute paths. The ones that
/// still exist are passed as [`FILES_VAR`], one per line, relative to the cwd when under it and
/// absolute otherwise. `{files}` in `cmd` becomes the same list, each path single-quoted for
/// the shell. Without files, or with too many to pass, [`FILES_VAR`] is unset and `{files}`
/// becomes the command's `auto.path` entries (`.` for the cwd itself, or when it has none).
#[must_use]
pub fn shell_invocation(
    cmd: &Command,
    fallback_cwd: &Path,
    files: Option<&[PathBuf]>,
) -> ShellInvocation {
    let cwd = cmd.effective_cwd(fallback_cwd).to_path_buf();
    let listed: Vec<OsString> = files
        .unwrap_or_default()
        .iter()
        .filter(|path| path.symlink_metadata().is_ok())
        .map(|path| relative_to_cwd(path, &cwd))
        .collect();

    let mut env: Vec<(OsString, OsString)> =
        cmd.env.iter().map(|(k, v)| (k.into(), v.into())).collect();
    let joined = join(listed.iter().map(|p| p.as_bytes()), b"\n");
    let passed = (!listed.is_empty() && joined.len() <= FILES_LIMIT).then_some(listed.as_slice());
    if passed.is_some() {
        env.retain(|(key, _)| key != FILES_VAR);
        env.push((FILES_VAR.into(), OsString::from_vec(joined)));
    }
    env.sort();

    let (script, listed_all) = expand_placeholder(cmd, &cwd, passed);
    let dropped = passed.is_none() || !listed_all;
    if !listed.is_empty() && dropped {
        warn!(
            "'{}' matched {} files, too many to pass: {FILES_VAR} is unset or {{files}} lists its \
             auto.path instead",
            cmd.name,
            listed.len()
        );
    }

    ShellInvocation {
        program: "sh",
        args: ["-c".into(), OsString::from_vec(script)],
        cwd,
        env,
    }
}

/// `cmd.cmd` with `{files}` replaced by `files`, quoted, or else by the command's `auto.path`
/// entries, as when there are no files or the script would get too long. Returns whether it
/// used `files`, as it does when there is no placeholder.
fn expand_placeholder(cmd: &Command, cwd: &Path, files: Option<&[OsString]>) -> (Vec<u8>, bool) {
    let template = cmd.cmd.as_bytes();
    if find(template, FILES_PLACEHOLDER).is_none() {
        return (template.to_vec(), true);
    }
    if let Some(files) = files {
        let quoted = join(files.iter().map(|p| quote(p)), b" ");
        let script = replace(template, FILES_PLACEHOLDER, &quoted);
        if script.len() <= FILES_LIMIT {
            return (script, true);
        }
    }
    let mut paths: Vec<OsString> = cmd
        .auto
        .paths()
        .iter()
        .map(|p| relative_to_cwd(p, cwd))
        .collect();
    if paths.is_empty() {
        paths.push(".".into());
    }
    let quoted = join(paths.iter().map(|p| quote(p)), b" ");
    (replace(template, FILES_PLACEHOLDER, &quoted), false)
}

/// `path` relative to `cwd` when under it (`.` for `cwd` itself), otherwise as it is. A relative
/// path starting with `-` gets `./`, so a tool doesn't take it for an option.
fn relative_to_cwd(path: &Path, cwd: &Path) -> OsString {
    match path.strip_prefix(cwd) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
        Ok(rel) if rel.as_os_str().as_bytes().starts_with(b"-") => {
            Path::new(".").join(rel).into_os_string()
        }
        Ok(rel) => rel.as_os_str().to_owned(),
        Err(_) => path.as_os_str().to_owned(),
    }
}

/// `s` single-quoted for a POSIX shell.
fn quote(s: &OsStr) -> Vec<u8> {
    let mut out = vec![b'\''];
    for &b in s.as_bytes() {
        if b == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(b);
        }
    }
    out.push(b'\'');
    out
}

fn join<I, S>(parts: I, sep: &[u8]) -> Vec<u8>
where
    I: IntoIterator<Item = S>,
    S: AsRef<[u8]>,
{
    let mut out = Vec::new();
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(sep);
        }
        out.extend_from_slice(part.as_ref());
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// `haystack` with every `needle` replaced by `with`.
fn replace(haystack: &[u8], needle: &[u8], with: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(haystack.len());
    let mut rest = haystack;
    while let Some(at) = find(rest, needle) {
        out.extend_from_slice(&rest[..at]);
        out.extend_from_slice(with);
        rest = &rest[at + needle.len()..];
    }
    out.extend_from_slice(rest);
    out
}

/// Make `command` start as the leader of a new session, and so of a new process group whose id
/// is its pid. It has no controlling terminal, so opening `/dev/tty` fails with `ENXIO` instead
/// of stopping it as a background job.
pub(crate) fn new_session(command: &mut std::process::Command) {
    // Not together with `process_group(0)`: setsid fails with EPERM in a group leader.
    // SAFETY: setsid is async-signal-safe, and the closure touches nothing else.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Kills a child (its process group, for a group handle) when dropped, unless disarmed, and
/// reaps it if it holds the exited [`Child`]. Covers panics, dropped futures and runtime
/// shutdown.
pub(crate) struct GroupGuard {
    handle: ProcessHandle,
    child: Option<Child>,
    armed: bool,
}

impl GroupGuard {
    pub(crate) fn new(handle: ProcessHandle) -> Self {
        Self {
            handle,
            child: None,
            armed: true,
        }
    }

    pub(crate) fn handle(&self) -> &ProcessHandle {
        &self.handle
    }

    /// Hand over the child once [`ProcessHandle::wait_exit`] has seen it exit.
    pub(crate) fn set_exited(&mut self, child: Child) {
        self.child = Some(child);
    }

    /// Reap the exited child without killing anything.
    pub(crate) fn reap(mut self) {
        self.armed = false;
        self.release();
    }

    fn release(&mut self) {
        if let Some(mut child) = self.child.take()
            && let Err(e) = self.handle.reap_with(|| child.wait())
        {
            warn!("Failed to reap pid {}: {e}", self.handle.pid());
        }
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if self.armed
            && let Err(e) = self.handle.force_kill()
        {
            warn!("Failed to kill pid {}: {e}", self.handle.pid());
        }
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(cmd: &str, cwd: &Path, paths: &[&Path]) -> Command {
        let mut command = Command {
            name: "lint".into(),
            cmd: cmd.into(),
            cwd: cwd.to_path_buf(),
            ..Command::default()
        };
        command.auto.path = Some(paths.iter().map(|p| p.to_path_buf()).collect());
        command
    }

    fn files_var(invocation: &ShellInvocation) -> Option<&OsStr> {
        invocation
            .env
            .iter()
            .find(|(k, _)| k == FILES_VAR)
            .map(|(_, v)| v.as_os_str())
    }

    fn script(invocation: &ShellInvocation) -> &OsStr {
        &invocation.args[1]
    }

    #[test]
    fn files_are_relative_quoted_and_existing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("src")).unwrap();
        let other = tempfile::tempdir().unwrap();
        let outside = other.path().join("x.py");
        for file in [
            root.join("src/it's.py"),
            root.join("src/a b.py"),
            outside.clone(),
        ] {
            std::fs::write(file, "").unwrap();
        }
        let files = [
            root.join("src/a b.py"),
            root.join("src/gone.py"),
            root.join("src/it's.py"),
            outside.clone(),
        ];
        let cmd = command("lint {files} && echo {files}", &root.join("src"), &[]);

        let invocation = shell_invocation(&cmd, root, Some(&files));
        let expected_list = format!("a b.py\nit's.py\n{}", outside.display());
        assert_eq!(files_var(&invocation), Some(OsStr::new(&expected_list)));
        let quoted = format!("'a b.py' 'it'\\''s.py' '{}'", outside.display());
        assert_eq!(
            script(&invocation),
            OsStr::new(&format!("lint {quoted} && echo {quoted}"))
        );
    }

    #[test]
    fn relative_paths_never_look_like_options() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("-x.py"), "").unwrap();
        let cmd = command("lint {files}", root, &[&root.join("-src")]);

        let invocation = shell_invocation(&cmd, root, Some(&[root.join("-x.py")]));
        assert_eq!(files_var(&invocation), Some(OsStr::new("./-x.py")));
        assert_eq!(script(&invocation), "lint './-x.py'");
        assert_eq!(script(&shell_invocation(&cmd, root, None)), "lint './-src'");
    }

    #[test]
    fn without_files_placeholder_lists_auto_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let cmd = command("lint {files}", root, &[&root.join("src"), root]);
        for files in [None, Some(&[root.join("src/deleted.py")][..])] {
            let invocation = shell_invocation(&cmd, root, files);
            assert_eq!(files_var(&invocation), None);
            assert_eq!(script(&invocation), "lint 'src' '.'");
        }

        let no_paths = command("lint {files}", root, &[]);
        let invocation = shell_invocation(&no_paths, root, None);
        assert_eq!(script(&invocation), "lint '.'");

        // A command without the placeholder runs as written
        let plain = command("lint", root, &[]);
        assert_eq!(script(&shell_invocation(&plain, root, None)), "lint");
    }

    #[test]
    fn oversize_file_list_is_not_passed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let create = |prefix: &str| -> Vec<PathBuf> {
            (0..600)
                .map(|i| {
                    let file = root.join(format!("{prefix}{i}"));
                    std::fs::write(&file, "").unwrap();
                    file
                })
                .collect()
        };
        let cmd = command("lint {files}", root, &[root]);

        let long_names = create(&"f".repeat(200));
        let invocation = shell_invocation(&cmd, root, Some(&long_names));
        assert_eq!(files_var(&invocation), None);
        assert_eq!(script(&invocation), "lint '.'");

        // Short enough for the variable, but not once each quote is escaped in the script
        let quotes = create(&"'".repeat(50));
        let invocation = shell_invocation(&cmd, root, Some(&quotes));
        assert!(files_var(&invocation).is_some());
        assert_eq!(script(&invocation), "lint '.'");
    }

    #[test]
    fn configured_files_var_is_replaced_only_by_a_list() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.py"), "").unwrap();
        let mut cmd = command("lint", root, &[]);
        cmd.env.insert(FILES_VAR.into(), "configured".into());

        let invocation = shell_invocation(&cmd, root, None);
        assert_eq!(files_var(&invocation), Some(OsStr::new("configured")));
        let invocation = shell_invocation(&cmd, root, Some(&[root.join("a.py")]));
        assert_eq!(files_var(&invocation), Some(OsStr::new("a.py")));
        assert_eq!(invocation.env.len(), 1);
    }
}
