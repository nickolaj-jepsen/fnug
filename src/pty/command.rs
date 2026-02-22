use log::debug;

use crate::commands::command::Command;
use portable_pty::CommandBuilder;

impl From<&Command> for CommandBuilder {
    fn from(command: &Command) -> Self {
        debug!(
            "Building command '{}' in {}",
            command.cmd,
            command.cwd.display()
        );
        let mut command_builder = CommandBuilder::new("sh");
        command_builder.args(["-c", &command.cmd]);
        for (key, value) in std::env::vars() {
            command_builder.env(key, value);
        }
        for (key, value) in &command.env {
            command_builder.env(key, value);
        }
        command_builder.env("TERM", "xterm-256color");
        command_builder.cwd(command.cwd.clone());
        command_builder
    }
}
