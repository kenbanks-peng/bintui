use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationBoundary {
    ManagedLinkCreation,
    ManagedLinkRemoval,
    LockAcquisition,
    TemporaryRegistryWrite,
    RegistrySync,
    AtomicRegistryReplacement,
    RegistryDirectorySync,
}

pub trait MutationFaultInjector: Send + Sync {
    fn check(&self, boundary: MutationBoundary) -> std::io::Result<()>;
}

pub struct NoMutationFaults;

impl MutationFaultInjector for NoMutationFaults {
    fn check(&self, _boundary: MutationBoundary) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Registration {
    pub name: String,
    pub target: PathBuf,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RegistrationDefectKind {
    TargetMissing,
    TargetNotExecutable,
    LinkMissing,
    LinkBroken,
    LinkIncorrect,
    Conflict,
}

impl RegistrationDefectKind {
    pub fn identifier(&self) -> &'static str {
        match self {
            Self::TargetMissing => "target-missing",
            Self::TargetNotExecutable => "target-not-executable",
            Self::LinkMissing => "link-missing",
            Self::LinkBroken => "link-broken",
            Self::LinkIncorrect => "link-incorrect",
            Self::Conflict => "conflict",
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RegistrationDefect {
    pub kind: RegistrationDefectKind,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ManagedPathKind {
    Missing,
    OwnedLink,
    SymbolicLink,
    File,
    Directory,
    Other,
}

impl ManagedPathKind {
    pub fn identifier(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::OwnedLink => "owned-link",
            Self::SymbolicLink => "symbolic-link",
            Self::File => "file",
            Self::Directory => "directory",
            Self::Other => "other",
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RegistrationState {
    pub registration: Registration,
    pub managed_link: PathBuf,
    pub actual: ManagedPathKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_link_target: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defect: Option<RegistrationDefect>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LifecycleStatus {
    Healthy,
    Blocked,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct LifecycleResult {
    pub status: LifecycleStatus,
    pub identifier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registration: Option<RegistrationState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ListResult {
    pub status: LifecycleStatus,
    pub identifier: String,
    pub registrations: Vec<RegistrationState>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictKind {
    CommandNameRegistered,
    DuplicateProposedName,
    ManagedPathOccupied,
}

impl ConflictKind {
    pub fn identifier(&self) -> &'static str {
        match self {
            Self::CommandNameRegistered => "command-name-registered",
            Self::DuplicateProposedName => "duplicate-proposed-name",
            Self::ManagedPathOccupied => "managed-path-occupied",
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct NameConflict {
    pub kind: ConflictKind,
    pub conflicting_target: PathBuf,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Candidate {
    pub proposed_name: String,
    pub target: PathBuf,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_candidate_registration"
    )]
    pub registration: Option<RegistrationState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict: Option<NameConflict>,
}

fn serialize_candidate_registration<S>(
    state: &Option<RegistrationState>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    state
        .as_ref()
        .map(|state| &state.registration)
        .serialize(serializer)
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WarningKind {
    UnreadableDirectory,
    UnreadablePath,
}

impl WarningKind {
    pub fn identifier(&self) -> &'static str {
        match self {
            Self::UnreadableDirectory => "unreadable-directory",
            Self::UnreadablePath => "unreadable-path",
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SearchWarning {
    pub kind: WarningKind,
    pub path: PathBuf,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SearchStatus {
    Healthy,
    Blocked,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SearchResult {
    pub status: SearchStatus,
    pub search_root: PathBuf,
    pub candidates: Vec<Candidate>,
    pub warnings: Vec<SearchWarning>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PathStatus {
    Present,
    Missing,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PathDiagnostic {
    pub status: PathStatus,
    pub identifier: String,
    pub bin_dir: PathBuf,
    pub matches: usize,
    pub guidance: String,
}

pub fn display_path(path: &Path, home: &Path) -> String {
    display_path_with_roots(path, home, &BTreeMap::new())
}

pub fn display_path_with_roots(
    path: &Path,
    home: &Path,
    roots: &BTreeMap<String, PathBuf>,
) -> String {
    let canonical_path = canonicalize_for_display(path);
    let mut best_match: Option<(&str, PathBuf, PathBuf, usize)> = None;
    for (name, configured_root) in roots {
        let (matched_path, root) = if path.strip_prefix(configured_root).is_ok() {
            (path.to_path_buf(), configured_root.clone())
        } else {
            let root = canonicalize_for_display(configured_root);
            if canonical_path.strip_prefix(&root).is_err() {
                continue;
            }
            (canonical_path.clone(), root)
        };
        let depth = root.components().count();
        if best_match
            .as_ref()
            .map_or(true, |(_, _, _, best_depth)| depth > *best_depth)
        {
            best_match = Some((name, matched_path, root, depth));
        }
    }
    if let Some((name, matched_path, root, _)) = best_match {
        let relative = matched_path
            .strip_prefix(&root)
            .expect("configured root was matched above");
        if relative.as_os_str().is_empty() {
            format!("[{name}]")
        } else {
            format!("[{name}]/{}", relative.display())
        }
    } else if !home.as_os_str().is_empty() && path == home {
        "~".to_owned()
    } else if !home.as_os_str().is_empty() {
        path.strip_prefix(home)
            .map(|relative| format!("~/{}", relative.display()))
            .unwrap_or_else(|_| path.display().to_string())
    } else {
        path.display().to_string()
    }
}

fn canonicalize_for_display(path: &Path) -> PathBuf {
    let mut unresolved = Vec::<OsString>::new();
    let mut existing = path.to_path_buf();
    loop {
        if let Ok(mut canonical) = fs::canonicalize(&existing) {
            for component in unresolved.iter().rev() {
                canonical.push(component);
            }
            return canonical;
        }
        let Some(name) = existing.file_name() else {
            return path.to_path_buf();
        };
        unresolved.push(name.to_owned());
        if !existing.pop() {
            return path.to_path_buf();
        }
    }
}
