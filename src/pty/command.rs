use log::debug;

use crate::commands::command::Command;
use crate::runner::process::{FILES_VAR, ShellInvocation, shell_invocation};
use portable_pty::CommandBuilder;

impl From<&ShellInvocation> for CommandBuilder {
    fn from(invocation: &ShellInvocation) -> Self {
        let mut command_builder = CommandBuilder::new(invocation.program);
        command_builder.args(&invocation.args);
        command_builder.env_remove(FILES_VAR);
        for (key, value) in &invocation.env {
            command_builder.env(key, value);
        }
        command_builder.env("TERM", "xterm-256color");
        // portable-pty silently falls back to $HOME for a missing cwd; spawn_pty rejects it first
        command_builder.cwd(&invocation.cwd);
        command_builder
    }
}

impl From<&Command> for CommandBuilder {
    /// The command without the files that selected it: `{files}` lists its `auto.path`.
    fn from(command: &Command) -> Self {
        debug!(
            "Building command '{}' in {}",
            command.cmd,
            command.cwd.display()
        );
        (&shell_invocation(command, &command.cwd, None)).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_runs_the_shell_invocation() {
        let dir = tempfile::tempdir().unwrap();
        let mut command = Command {
            cmd: "lint {files}".into(),
            cwd: dir.path().to_path_buf(),
            ..Command::default()
        };
        command.env.insert("MODE".into(), "strict".into());
        command.auto.path = Some(vec![dir.path().join("src")]);

        let builder = CommandBuilder::from(&command);
        assert_eq!(builder.get_argv(), &["sh", "-c", "lint 'src'"]);
        assert_eq!(builder.get_env("MODE"), Some("strict".as_ref()));
        assert_eq!(builder.get_env(FILES_VAR), None);
        assert_eq!(builder.get_env("TERM"), Some("xterm-256color".as_ref()));
        assert_eq!(builder.get_cwd(), Some(&dir.path().as_os_str().to_owned()));
    }
}
