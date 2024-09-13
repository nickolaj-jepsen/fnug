use crate::command_group::Command;
use git2::Repository;
use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum GitError {
    #[error("Unable to find git repository for path: {0}")]
    NoGitRepo(PathBuf),
    #[error("Invalid regex")]
    Regex(#[from] regex::Error),
}

fn find_git_repos(paths: &Vec<PathBuf>) -> Result<Vec<Repository>, GitError> {
    let mut git_repos: Vec<Repository> = Vec::new();
    for path in paths {
        let repo = match Repository::discover(path) {
            Ok(repo) => repo,
            Err(_) => return Err(GitError::NoGitRepo(path.clone())),
        };
        git_repos.push(repo);
    }
    Ok(git_repos)
}

pub fn commands_with_changes(commands: Vec<&Command>) -> Result<Vec<&Command>, GitError> {
    let (always_commands, remaining_commands): (Vec<&Command>, Vec<&Command>) =
        commands.iter().partition(|command| command.auto.always);
    let auto_commands: Vec<&Command> = remaining_commands
        .into_iter()
        .filter(|command| command.auto.git)
        .collect();

    // Group all auto commands by their regex and path Vec
    let mut grouped_commands: HashMap<(&Vec<PathBuf>, &Vec<String>), Vec<&Command>> =
        HashMap::new();
    for command in auto_commands {
        let key = (&command.auto.path, &command.auto.regex);
        grouped_commands.entry(key).or_default().push(command);
    }

    // Find all git repositories for each group
    let mut changed_commands: Vec<&Command> = Vec::new();
    for (key, commands) in grouped_commands {
        let (paths, regexes) = key;
        let git_repos = find_git_repos(paths)?;
        for repo in git_repos {
            let statuses = repo.statuses(None).unwrap();
            for status in statuses.iter() {
                for command in &commands {
                    for regex in regexes {
                        let re = regex::Regex::new(regex).map_err(GitError::from)?;
                        if re.is_match(status.path().unwrap()) {
                            changed_commands.push(command);
                        }
                    }
                }
            }
        }
    }

    Ok(changed_commands
        .into_iter()
        .chain(always_commands)
        .collect())
}
