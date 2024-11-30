use crate::commands::command::Command;
use portable_pty::CommandBuilder;

impl From<&Command> for CommandBuilder {
    fn from(command: &Command) -> Self {
        let command_parts: Vec<&str> = command.cmd.split_whitespace().collect();
        let (cmd, args) = command_parts.split_at_checked(1).unwrap_or((&[""], &[]));
        let mut command_builder = CommandBuilder::new(cmd[0]);
        command_builder.env("TERM", "xterm-256color");
        for (key, value) in std::env::vars() {
            command_builder.env(key, value);
        }
        command_builder.args(args);
        command_builder.cwd(command.cwd.clone());
        command_builder
    }
}
