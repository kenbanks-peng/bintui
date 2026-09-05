use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use thiserror::Error;

use crate::environment::Environment;

const COMMIT_MESSAGE: &str = "Update bintui configuration";

#[derive(Debug, Error)]
pub enum ConfigGitError {
    #[error("configuration change {path} is outside XDG_CONFIG_HOME {config_home}")]
    OutsideConfigHome { path: PathBuf, config_home: PathBuf },
    #[error("configuration change path is not valid UTF-8: {0}")]
    NonUtf8Path(PathBuf),
    #[error("could not resolve configuration change {path}: {source}")]
    Resolve {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not run git while {action} in {config_home}: {source}")]
    Execute {
        action: &'static str,
        config_home: PathBuf,
        source: std::io::Error,
    },
    #[error("git failed while {action} in {config_home}: {message}")]
    Failed {
        action: &'static str,
        config_home: PathBuf,
        message: String,
    },
}

/// Commits and pushes one file changed by bintui when XDG_CONFIG_HOME is itself a Git worktree.
/// Other staged or unstaged files are left out of the commit.
pub fn commit_and_push(
    changed_path: &Path,
    environment: &Environment,
) -> Result<(), ConfigGitError> {
    commit_and_push_in(changed_path, &config_home(environment))
}

pub(crate) fn config_home(environment: &Environment) -> PathBuf {
    environment
        .variable("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| environment.home().join(".config"))
}

pub(crate) fn commit_and_push_in(
    changed_path: &Path,
    config_home: &Path,
) -> Result<(), ConfigGitError> {
    changed_path
        .strip_prefix(config_home)
        .map_err(|_| ConfigGitError::OutsideConfigHome {
            path: changed_path.to_path_buf(),
            config_home: config_home.to_path_buf(),
        })?;
    let physical_path =
        fs::canonicalize(changed_path).map_err(|source| ConfigGitError::Resolve {
            path: changed_path.to_path_buf(),
            source,
        })?;
    let Some(repository) = physical_path
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
    else {
        return Ok(());
    };
    let relative = physical_path
        .strip_prefix(repository)
        .expect("resolved configuration path is inside its Git worktree");
    let relative = relative
        .to_str()
        .ok_or_else(|| ConfigGitError::NonUtf8Path(relative.to_path_buf()))?;

    run_git(
        repository,
        "staging the bintui change",
        &["add", "--", relative],
    )?;
    let diff = git_output(
        repository,
        "checking the staged bintui change",
        &["diff", "--cached", "--quiet", "--", relative],
    )?;
    match diff.status.code() {
        Some(0) => return Ok(()),
        Some(1) => {}
        _ => {
            return Err(command_failure(
                "checking the staged bintui change",
                repository,
                &diff,
            ))
        }
    }
    run_git(
        repository,
        "committing the bintui change",
        &["commit", "--only", "-m", COMMIT_MESSAGE, "--", relative],
    )?;
    run_git(repository, "pushing the bintui change", &["push"])?;
    Ok(())
}

fn run_git(
    config_home: &Path,
    action: &'static str,
    arguments: &[&str],
) -> Result<(), ConfigGitError> {
    let output = git_output(config_home, action, arguments)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failure(action, config_home, &output))
    }
}

fn git_output(
    config_home: &Path,
    action: &'static str,
    arguments: &[&str],
) -> Result<Output, ConfigGitError> {
    Command::new("git")
        .arg("-C")
        .arg(config_home)
        .args(arguments)
        .output()
        .map_err(|source| ConfigGitError::Execute {
            action,
            config_home: config_home.to_path_buf(),
            source,
        })
}

fn command_failure(action: &'static str, config_home: &Path, output: &Output) -> ConfigGitError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let message = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("git exited with {}", output.status)
    };
    ConfigGitError::Failed {
        action,
        config_home: config_home.to_path_buf(),
        message,
    }
}
