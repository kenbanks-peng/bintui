use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Environment {
    home: PathBuf,
    cwd: PathBuf,
    variables: BTreeMap<String, OsString>,
}

impl Environment {
    pub fn current() -> Result<Self, env::VarError> {
        let variables = env::vars_os()
            .filter_map(|(key, value)| key.into_string().ok().map(|key| (key, value)))
            .collect();
        let home = PathBuf::from(env::var_os("HOME").ok_or(env::VarError::NotPresent)?);
        Ok(Self {
            home: std::fs::canonicalize(&home).unwrap_or(home),
            cwd: env::current_dir().map_err(|_| env::VarError::NotPresent)?,
            variables,
        })
    }

    pub fn from_values(home: PathBuf, cwd: PathBuf, variables: BTreeMap<String, OsString>) -> Self {
        Self {
            home,
            cwd,
            variables,
        }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn variable(&self, name: &str) -> Option<&OsStr> {
        self.variables.get(name).map(OsString::as_os_str)
    }
}
