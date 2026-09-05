//! Shared application policy for discovery and single-Registration lifecycle operations.
//!
//! Mutations cannot atomically update both a Registry file and a Managed Link. Enabled add and
//! enable therefore create the safe link before atomically persisting desired state; disable and
//! remove first verify and remove only an owned link, then persist. A failed pre-commit Registry
//! replacement rolls the link operation back when it is still safe to do so. Once replacement has
//! committed, including a later directory-sync failure, the new Registry state is authoritative
//! and the link is not rolled back. Every operation holds the Registry writer lock while it
//! revalidates both resources, so stale observations never authorize a mutation.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::config_git::ConfigGitSync;
use crate::configuration::{self, normalize, ConfigurationError};
use crate::discovery;
use crate::discovery_ignore::{self, DiscoveryIgnoreError};
use crate::environment::Environment;
use crate::model::{
    Candidate, ConflictKind, LifecycleResult, LifecycleStatus, ListResult, ManagedPathKind,
    MutationBoundary, MutationFaultInjector, NameConflict, NoMutationFaults, PathDiagnostic,
    PathStatus, Registration, RegistrationDefect, RegistrationDefectKind, RegistrationState,
    SearchResult, SearchStatus,
};
use crate::registry::{self, LockedRegistry, RegistryError};

#[derive(Clone, Debug)]
pub struct SearchRequest {
    pub search_root: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct AddRequest {
    pub target: PathBuf,
    pub name: Option<String>,
    pub disabled: bool,
}

pub struct Application<'a> {
    environment: &'a Environment,
    faults: &'a dyn MutationFaultInjector,
    git_sync: ConfigGitSync,
}

impl<'a> Application<'a> {
    pub fn new(environment: &'a Environment) -> Self {
        Self {
            environment,
            faults: &NoMutationFaults,
            git_sync: ConfigGitSync::blocking(),
        }
    }

    pub fn with_git_sync(environment: &'a Environment, git_sync: ConfigGitSync) -> Self {
        Self {
            environment,
            faults: &NoMutationFaults,
            git_sync,
        }
    }

    pub fn with_faults(
        environment: &'a Environment,
        faults: &'a dyn MutationFaultInjector,
    ) -> Self {
        Self {
            environment,
            faults,
            git_sync: ConfigGitSync::blocking(),
        }
    }

    pub fn validate_add(&self, request: AddRequest) -> Result<LifecycleResult, ApplicationError> {
        validate_add(request, self.environment)
    }

    pub fn add(&self, request: AddRequest) -> Result<LifecycleResult, ApplicationError> {
        let resulting_name = request.name.clone().or_else(|| {
            request
                .target
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        });
        add_with_faults(request, self.environment, self.faults, &self.git_sync).map_err(|error| {
            mutation_error(
                "add-failed",
                error,
                resulting_name.as_deref(),
                self.environment,
            )
        })
    }

    pub fn enable(&self, name: &str) -> Result<LifecycleResult, ApplicationError> {
        set_enabled(name, true, self.environment, self.faults, &self.git_sync)
            .map_err(|error| mutation_error("enable-failed", error, Some(name), self.environment))
    }

    pub fn disable(&self, name: &str) -> Result<LifecycleResult, ApplicationError> {
        set_enabled(name, false, self.environment, self.faults, &self.git_sync)
            .map_err(|error| mutation_error("disable-failed", error, Some(name), self.environment))
    }

    pub fn remove(&self, name: &str) -> Result<LifecycleResult, ApplicationError> {
        remove_with_faults(name, self.environment, self.faults, &self.git_sync)
            .map_err(|error| mutation_error("remove-failed", error, Some(name), self.environment))
    }

    pub fn rename(&self, name: &str, new_name: &str) -> Result<LifecycleResult, ApplicationError> {
        rename_with_faults(
            name,
            new_name,
            self.environment,
            self.faults,
            &self.git_sync,
        )
        .map_err(|error| rename_error(error, name, new_name, self.environment))
    }

    pub fn ignore_target(&self, target: &Path) -> Result<String, ApplicationError> {
        ignore_target_with_git_sync(target, self.environment, &self.git_sync)
    }

    pub fn path_diagnostic(&self) -> Result<PathDiagnostic, ApplicationError> {
        path_diagnostic(self.environment)
    }
}

#[derive(Debug, Error)]
pub enum ApplicationError {
    #[error("{0}")]
    Configuration(#[from] ConfigurationError),
    #[error("{0}")]
    Registry(#[from] RegistryError),
    #[error("{0}")]
    DiscoveryIgnore(#[from] DiscoveryIgnoreError),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("unknown Command Name {0:?}")]
    UnknownName(String),
    #[error("could not search {path}: {source}")]
    Search {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not inspect Target {path}: {source}")]
    InspectTarget {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not inspect managed path {path}: {source}")]
    InspectManagedPath {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not create managed directory {path}: {source}")]
    CreateManagedDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not create Managed Link {path}: {source}")]
    CreateManagedLink {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not remove Managed Link {path}: {source}")]
    RemoveManagedLink {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{message}")]
    Operation {
        identifier: &'static str,
        message: String,
        resulting_registration: Option<Box<RegistrationState>>,
    },
    #[error("{message}")]
    Mutation {
        identifier: &'static str,
        message: String,
        resulting_registration: Option<Box<RegistrationState>>,
    },
}

impl ApplicationError {
    pub fn identifier(&self) -> &'static str {
        match self {
            Self::Operation { identifier, .. } | Self::Mutation { identifier, .. } => identifier,
            _ => "operation-failed",
        }
    }

    pub fn resulting_registration(&self) -> Option<&RegistrationState> {
        match self {
            Self::Operation {
                resulting_registration,
                ..
            }
            | Self::Mutation {
                resulting_registration,
                ..
            } => resulting_registration.as_deref(),
            _ => None,
        }
    }
}

fn rename_error(
    error: ApplicationError,
    name: &str,
    new_name: &str,
    environment: &Environment,
) -> ApplicationError {
    let configuration = configuration::load(environment).ok();
    let path_state = configuration.as_ref().map(|configuration| {
        let old = configuration.bin_dir.join(name);
        let new = configuration.bin_dir.join(new_name);
        format!(
            "; actual paths after failure: old {} is {}, new {} is {}; next safe action: resolve any reported conflict, run `bin list`, then retry",
            old.display(),
            managed_path_summary(&old),
            new.display(),
            managed_path_summary(&new)
        )
    });
    let mut wrapped = mutation_error("rename-failed", error, Some(new_name), environment);
    if let ApplicationError::Mutation {
        message,
        resulting_registration,
        ..
    } = &mut wrapped
    {
        if resulting_registration.is_none() {
            *resulting_registration = configuration.and_then(|configuration| {
                let registrations = registry::load(environment).ok()?;
                let registration = registrations
                    .iter()
                    .find(|registration| registration.name == name)?;
                inspect_registration(registration, &configuration.bin_dir)
                    .ok()
                    .map(Box::new)
            });
        }
        if let Some(path_state) = path_state {
            message.push_str(&path_state);
        }
    }
    wrapped
}

fn managed_path_summary(path: &Path) -> &'static str {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "missing",
        Ok(metadata) if metadata.file_type().is_symlink() => "a symbolic link",
        Ok(metadata) if metadata.is_file() => "an unmanaged file",
        Ok(metadata) if metadata.is_dir() => "an unmanaged directory",
        Ok(_) => "an unmanaged entry",
        Err(_) => "unreadable",
    }
}

fn mutation_error(
    identifier: &'static str,
    error: ApplicationError,
    name: Option<&str>,
    environment: &Environment,
) -> ApplicationError {
    let resulting_registration = name.and_then(|name| {
        let configuration = configuration::load(environment).ok()?;
        let registrations = registry::load(environment).ok()?;
        let registration = registrations
            .iter()
            .find(|registration| registration.name == name)?;
        inspect_registration(registration, &configuration.bin_dir)
            .ok()
            .map(Box::new)
    });
    ApplicationError::Mutation {
        identifier,
        message: error.to_string(),
        resulting_registration,
    }
}

fn status_from_defects(states: &[RegistrationState]) -> LifecycleStatus {
    if states.iter().all(|state| state.defect.is_none()) {
        LifecycleStatus::Healthy
    } else {
        LifecycleStatus::Blocked
    }
}

pub fn list(environment: &Environment) -> Result<ListResult, ApplicationError> {
    let configuration = configuration::load(environment)?;
    let registrations = registry::load(environment)?;
    let states = inspect_registrations(&registrations, &configuration.bin_dir)?;
    if !states.iter().any(|state| {
        state.defect.as_ref().map(|defect| &defect.kind)
            == Some(&RegistrationDefectKind::LinkMissing)
    }) {
        return Ok(list_result(states));
    }

    // A missing Managed Link is absent generated state, not an ownership conflict. Revalidate
    // under the Registry writer lock before restoring it so concurrent lifecycle operations cannot
    // make a stale observation authorize the mutation.
    let locked_registry = LockedRegistry::acquire(environment, &NoMutationFaults)?;
    let locked_states =
        inspect_registrations(locked_registry.registrations(), &configuration.bin_dir)?;
    let missing_links = locked_states
        .iter()
        .filter(|state| {
            state.defect.as_ref().map(|defect| &defect.kind)
                == Some(&RegistrationDefectKind::LinkMissing)
        })
        .collect::<Vec<_>>();
    if !missing_links.is_empty() {
        fs::create_dir_all(&configuration.bin_dir).map_err(|source| {
            ApplicationError::CreateManagedDirectory {
                path: configuration.bin_dir.clone(),
                source,
            }
        })?;
        for state in missing_links {
            if let Err(source) = symlink(&state.registration.target, &state.managed_link) {
                if source.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(ApplicationError::CreateManagedLink {
                        path: state.managed_link.clone(),
                        source,
                    });
                }
            }
        }
    }

    Ok(list_result(inspect_registrations(
        locked_registry.registrations(),
        &configuration.bin_dir,
    )?))
}

fn inspect_registrations(
    registrations: &[Registration],
    bin_dir: &Path,
) -> Result<Vec<RegistrationState>, ApplicationError> {
    registrations
        .iter()
        .map(|registration| inspect_registration(registration, bin_dir))
        .collect()
}

fn list_result(registrations: Vec<RegistrationState>) -> ListResult {
    ListResult {
        status: status_from_defects(&registrations),
        identifier: "registrations-listed".to_owned(),
        registrations,
    }
}

pub fn enable(name: &str, environment: &Environment) -> Result<LifecycleResult, ApplicationError> {
    Application::new(environment).enable(name)
}

pub fn disable(name: &str, environment: &Environment) -> Result<LifecycleResult, ApplicationError> {
    Application::new(environment).disable(name)
}

fn set_enabled(
    name: &str,
    enabled: bool,
    environment: &Environment,
    faults: &dyn MutationFaultInjector,
    git_sync: &ConfigGitSync,
) -> Result<LifecycleResult, ApplicationError> {
    registry::validate_name(name).map_err(ApplicationError::InvalidInput)?;
    let configuration = configuration::load(environment)?;
    let mut registry = LockedRegistry::acquire(environment, faults)?;
    let existing = registry
        .registrations()
        .iter()
        .find(|registration| registration.name == name)
        .cloned()
        .ok_or_else(|| ApplicationError::UnknownName(name.to_owned()))?;
    let managed_link = managed_link(&configuration.bin_dir, name)?;
    let path_exists = fs::symlink_metadata(&managed_link).is_ok();
    let owned = is_owned_link(&managed_link, &existing.target);
    if path_exists && !owned {
        return blocked(existing, &configuration.bin_dir, "managed-path-conflict");
    }
    if existing.enabled == enabled && ((enabled && owned) || (!enabled && !path_exists)) {
        return lifecycle_result(
            if enabled {
                "registration-enabled"
            } else {
                "registration-disabled"
            },
            existing,
            &configuration.bin_dir,
        );
    }
    if enabled {
        validate_target(&existing.target)?;
    }
    if enabled && !owned {
        fs::create_dir_all(&configuration.bin_dir).map_err(|source| {
            ApplicationError::CreateManagedDirectory {
                path: configuration.bin_dir.clone(),
                source,
            }
        })?;
        faults
            .check(MutationBoundary::ManagedLinkCreation)
            .map_err(|source| ApplicationError::CreateManagedLink {
                path: managed_link.clone(),
                source,
            })?;
        symlink(&existing.target, &managed_link).map_err(|source| {
            ApplicationError::CreateManagedLink {
                path: managed_link.clone(),
                source,
            }
        })?;
    } else if !enabled && owned {
        remove_owned_link(&managed_link, &existing.target, faults)?;
    }
    let mut updated = existing.clone();
    updated.enabled = enabled;
    let mut registrations = registry.registrations().to_vec();
    let position = registrations
        .iter()
        .position(|registration| registration.name == name)
        .expect("locked Registry still contains Registration");
    registrations[position] = updated.clone();
    if let Err(error) = registry.replace(registrations, faults, git_sync, environment) {
        if !error.replacement_committed() {
            if enabled {
                let _ = remove_owned_link(&managed_link, &existing.target, &NoMutationFaults);
            } else if !enabled && fs::symlink_metadata(&managed_link).is_err() {
                let _ = symlink(&existing.target, &managed_link);
            }
        }
        return Err(error.into());
    }
    lifecycle_result(
        if enabled {
            "registration-enabled"
        } else {
            "registration-disabled"
        },
        updated,
        &configuration.bin_dir,
    )
}

pub fn remove(name: &str, environment: &Environment) -> Result<LifecycleResult, ApplicationError> {
    Application::new(environment).remove(name)
}

fn remove_with_faults(
    name: &str,
    environment: &Environment,
    faults: &dyn MutationFaultInjector,
    git_sync: &ConfigGitSync,
) -> Result<LifecycleResult, ApplicationError> {
    registry::validate_name(name).map_err(ApplicationError::InvalidInput)?;
    let configuration = configuration::load(environment)?;
    let mut registry = LockedRegistry::acquire(environment, faults)?;
    let existing = registry
        .registrations()
        .iter()
        .find(|registration| registration.name == name)
        .cloned()
        .ok_or_else(|| ApplicationError::UnknownName(name.to_owned()))?;
    let managed_link = managed_link(&configuration.bin_dir, name)?;
    let path_exists = fs::symlink_metadata(&managed_link).is_ok();
    let owned = is_owned_link(&managed_link, &existing.target);
    if path_exists && !owned {
        return blocked(existing, &configuration.bin_dir, "managed-path-conflict");
    }
    if owned {
        remove_owned_link(&managed_link, &existing.target, faults)?;
    }
    let registrations = registry
        .registrations()
        .iter()
        .filter(|registration| registration.name != name)
        .cloned()
        .collect();
    if let Err(error) = registry.replace(registrations, faults, git_sync, environment) {
        if !error.replacement_committed() && owned && fs::symlink_metadata(&managed_link).is_err() {
            let _ = symlink(&existing.target, &managed_link);
        }
        return Err(error.into());
    }
    Ok(LifecycleResult {
        status: LifecycleStatus::Healthy,
        identifier: "registration-removed".to_owned(),
        registration: None,
        conflict: None,
    })
}

fn lifecycle_result(
    identifier: &str,
    registration: Registration,
    bin_dir: &Path,
) -> Result<LifecycleResult, ApplicationError> {
    let state = inspect_registration(&registration, bin_dir)?;
    let status = status_from_defects(std::slice::from_ref(&state));
    Ok(LifecycleResult {
        status,
        identifier: identifier.to_owned(),
        registration: Some(state),
        conflict: None,
    })
}

fn blocked(
    registration: Registration,
    bin_dir: &Path,
    identifier: &str,
) -> Result<LifecycleResult, ApplicationError> {
    let state = inspect_registration(&registration, bin_dir)?;
    Ok(LifecycleResult {
        status: LifecycleStatus::Blocked,
        identifier: identifier.to_owned(),
        conflict: state.defect.as_ref().map(|defect| defect.message.clone()),
        registration: Some(state),
    })
}

pub fn validate_add(
    request: AddRequest,
    environment: &Environment,
) -> Result<LifecycleResult, ApplicationError> {
    let prepared = prepare_add(request, environment)?;
    let registrations = registry::load(environment)?;
    if let Some(blocked) = add_conflict(&prepared, &registrations)? {
        return Ok(blocked);
    }
    Ok(LifecycleResult {
        status: LifecycleStatus::Healthy,
        identifier: "add-valid".to_owned(),
        registration: None,
        conflict: None,
    })
}

struct PreparedAdd {
    target: PathBuf,
    name: String,
    disabled: bool,
    bin_dir: PathBuf,
    managed_link: PathBuf,
}

fn prepare_add(
    request: AddRequest,
    environment: &Environment,
) -> Result<PreparedAdd, ApplicationError> {
    let configuration = configuration::load(environment)?;
    let target = absolute_target(&request.target, environment);
    validate_target(&target)?;
    let name = match request.name {
        Some(name) => name,
        None => target
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .ok_or_else(|| {
                ApplicationError::InvalidInput(
                    "Target must have a UTF-8 filename for its default Command Name".to_owned(),
                )
            })?,
    };
    registry::validate_name(&name).map_err(ApplicationError::InvalidInput)?;
    let managed_link = managed_link(&configuration.bin_dir, &name)?;
    Ok(PreparedAdd {
        target,
        name,
        disabled: request.disabled,
        bin_dir: configuration.bin_dir,
        managed_link,
    })
}

fn add_conflict(
    prepared: &PreparedAdd,
    registrations: &[Registration],
) -> Result<Option<LifecycleResult>, ApplicationError> {
    if let Some(existing) = registrations
        .iter()
        .find(|registration| registration.name == prepared.name)
    {
        return Ok(Some(LifecycleResult {
            status: LifecycleStatus::Blocked,
            identifier: "command-name-registered".to_owned(),
            registration: Some(inspect_registration(existing, &prepared.bin_dir)?),
            conflict: Some(format!(
                "Command Name {:?} is already registered to {}",
                prepared.name,
                existing.target.display()
            )),
        }));
    }
    match fs::symlink_metadata(&prepared.managed_link) {
        Ok(_) => Ok(Some(LifecycleResult {
            status: LifecycleStatus::Blocked,
            identifier: "managed-path-occupied".to_owned(),
            registration: None,
            conflict: Some(format!(
                "managed path {} is occupied by an unmanaged entry",
                prepared.managed_link.display()
            )),
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ApplicationError::InspectManagedPath {
            path: prepared.managed_link.clone(),
            source,
        }),
    }
}

pub fn add(
    request: AddRequest,
    environment: &Environment,
) -> Result<LifecycleResult, ApplicationError> {
    Application::new(environment).add(request)
}

fn add_with_faults(
    request: AddRequest,
    environment: &Environment,
    faults: &dyn MutationFaultInjector,
    git_sync: &ConfigGitSync,
) -> Result<LifecycleResult, ApplicationError> {
    let prepared = prepare_add(request, environment)?;
    let mut registry = LockedRegistry::acquire(environment, faults)?;
    if let Some(blocked) = add_conflict(&prepared, registry.registrations())? {
        return Ok(blocked);
    }
    let managed_link = prepared.managed_link;
    let bin_dir = prepared.bin_dir;
    let registration = Registration {
        name: prepared.name,
        target: prepared.target,
        enabled: !prepared.disabled,
    };
    if registration.enabled {
        fs::create_dir_all(&bin_dir).map_err(|source| {
            ApplicationError::CreateManagedDirectory {
                path: bin_dir.clone(),
                source,
            }
        })?;
        faults
            .check(MutationBoundary::ManagedLinkCreation)
            .map_err(|source| ApplicationError::CreateManagedLink {
                path: managed_link.clone(),
                source,
            })?;
        symlink(&registration.target, &managed_link).map_err(|source| {
            ApplicationError::CreateManagedLink {
                path: managed_link.clone(),
                source,
            }
        })?;
    }
    let mut registrations = registry.registrations().to_vec();
    registrations.push(registration.clone());
    registrations.sort_by(|left, right| left.name.cmp(&right.name));
    if let Err(error) = registry.replace(registrations, faults, git_sync, environment) {
        if !error.replacement_committed() && registration.enabled {
            let _ = remove_owned_link(&managed_link, &registration.target, &NoMutationFaults);
        }
        return Err(error.into());
    }
    Ok(LifecycleResult {
        status: LifecycleStatus::Healthy,
        identifier: "registration-added".to_owned(),
        registration: Some(inspect_registration(&registration, &bin_dir)?),
        conflict: None,
    })
}

fn absolute_target(target: &Path, environment: &Environment) -> PathBuf {
    if target.is_absolute() {
        normalize(target)
    } else {
        normalize(&environment.cwd().join(target))
    }
}

fn validate_target(target: &Path) -> Result<(), ApplicationError> {
    registry::validate_target_path(target).map_err(ApplicationError::InvalidInput)?;
    let metadata = fs::metadata(target).map_err(|error| {
        let message = if error.kind() == std::io::ErrorKind::NotFound {
            format!("Target does not exist: {}", target.display())
        } else {
            format!("could not inspect Target {}: {error}", target.display())
        };
        ApplicationError::InvalidInput(message)
    })?;
    if !metadata.is_file() {
        return Err(ApplicationError::InvalidInput(format!(
            "Target is not a file: {}",
            target.display()
        )));
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(ApplicationError::InvalidInput(format!(
            "Target is not executable: {}",
            target.display()
        )));
    }
    Ok(())
}

fn managed_link(bin_dir: &Path, name: &str) -> Result<PathBuf, ApplicationError> {
    let path = normalize(&bin_dir.join(name));
    if path.parent() != Some(bin_dir) {
        return Err(ApplicationError::InvalidInput(format!(
            "Command Name {name:?} escapes managed bin directory"
        )));
    }
    Ok(path)
}

fn inspect_registration(
    registration: &Registration,
    bin_dir: &Path,
) -> Result<RegistrationState, ApplicationError> {
    let managed_link = managed_link(bin_dir, &registration.name)?;
    let observed_link_target = fs::read_link(&managed_link).ok();
    let actual = match fs::symlink_metadata(&managed_link) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ManagedPathKind::Missing,
        Err(source) => {
            return Err(ApplicationError::InspectManagedPath {
                path: managed_link,
                source,
            })
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if is_owned_link(&managed_link, &registration.target) {
                ManagedPathKind::OwnedLink
            } else {
                ManagedPathKind::SymbolicLink
            }
        }
        Ok(metadata) if metadata.is_file() => ManagedPathKind::File,
        Ok(metadata) if metadata.is_dir() => ManagedPathKind::Directory,
        Ok(_) => ManagedPathKind::Other,
    };
    let defect =
        classify_defect(registration, &managed_link, &actual)?.map(|kind| RegistrationDefect {
            message: defect_message(&kind, registration.enabled, &actual).to_owned(),
            kind,
        });
    Ok(RegistrationState {
        registration: registration.clone(),
        managed_link,
        actual,
        observed_link_target,
        defect,
    })
}

fn classify_defect(
    registration: &Registration,
    managed_link: &Path,
    actual: &ManagedPathKind,
) -> Result<Option<RegistrationDefectKind>, ApplicationError> {
    match fs::metadata(&registration.target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some(RegistrationDefectKind::TargetMissing));
        }
        Err(source) => {
            return Err(ApplicationError::InspectTarget {
                path: registration.target.clone(),
                source,
            });
        }
        Ok(metadata) if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 => {
            return Ok(Some(RegistrationDefectKind::TargetNotExecutable));
        }
        Ok(_) => {}
    }

    if !registration.enabled {
        return Ok(
            (*actual != ManagedPathKind::Missing).then_some(RegistrationDefectKind::Conflict)
        );
    }

    match actual {
        ManagedPathKind::OwnedLink => Ok(None),
        ManagedPathKind::Missing => Ok(Some(RegistrationDefectKind::LinkMissing)),
        ManagedPathKind::SymbolicLink => match fs::metadata(managed_link) {
            Ok(_) => Ok(Some(RegistrationDefectKind::LinkIncorrect)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(Some(RegistrationDefectKind::LinkBroken))
            }
            Err(source) => Err(ApplicationError::InspectManagedPath {
                path: managed_link.to_path_buf(),
                source,
            }),
        },
        _ => Ok(Some(RegistrationDefectKind::Conflict)),
    }
}

fn defect_message(
    kind: &RegistrationDefectKind,
    enabled: bool,
    actual: &ManagedPathKind,
) -> &'static str {
    match kind {
        RegistrationDefectKind::TargetMissing => {
            "Target does not exist; restore it or remove the Registration"
        }
        RegistrationDefectKind::TargetNotExecutable => {
            "Target is not an executable regular file; restore executable permissions"
        }
        RegistrationDefectKind::LinkMissing => "Enabled Registration has no Managed Link",
        RegistrationDefectKind::Conflict if !enabled && *actual == ManagedPathKind::OwnedLink => {
            "Disabled Registration still has its owned Managed Link"
        }
        RegistrationDefectKind::LinkBroken => {
            "Managed Link points to a missing unexpected Target and ownership cannot be proven"
        }
        RegistrationDefectKind::LinkIncorrect => {
            "Managed Link points to an unexpected Target and ownership cannot be proven"
        }
        RegistrationDefectKind::Conflict => {
            "Managed path contains an entry the executable registry cannot prove it owns"
        }
    }
}

fn is_owned_link(path: &Path, target: &Path) -> bool {
    fs::read_link(path)
        .map(|actual| actual == target)
        .unwrap_or(false)
}

fn remove_owned_link(
    path: &Path,
    target: &Path,
    faults: &dyn MutationFaultInjector,
) -> Result<(), ApplicationError> {
    faults
        .check(MutationBoundary::ManagedLinkRemoval)
        .map_err(|source| ApplicationError::RemoveManagedLink {
            path: path.to_path_buf(),
            source,
        })?;
    if !is_owned_link(path, target) {
        return Err(ApplicationError::InvalidInput(format!(
            "Managed Link {} changed before removal; it was left untouched; inspect the path and retry",
            path.display()
        )));
    }
    fs::remove_file(path).map_err(|source| ApplicationError::RemoveManagedLink {
        path: path.to_path_buf(),
        source,
    })
}

fn rename_paths_summary(old_link: &Path, new_link: &Path) -> String {
    format!(
        "actual paths: old {} is {}, new {} is {}",
        old_link.display(),
        managed_path_summary(old_link),
        new_link.display(),
        managed_path_summary(new_link)
    )
}

pub fn rename(
    name: &str,
    new_name: &str,
    environment: &Environment,
) -> Result<LifecycleResult, ApplicationError> {
    Application::new(environment).rename(name, new_name)
}

fn rename_with_faults(
    name: &str,
    new_name: &str,
    environment: &Environment,
    faults: &dyn MutationFaultInjector,
    git_sync: &ConfigGitSync,
) -> Result<LifecycleResult, ApplicationError> {
    registry::validate_name(name).map_err(ApplicationError::InvalidInput)?;
    registry::validate_name(new_name).map_err(ApplicationError::InvalidInput)?;
    let configuration = configuration::load(environment)?;
    let mut registry = LockedRegistry::acquire(environment, faults)?;
    let existing = registry
        .registrations()
        .iter()
        .find(|registration| registration.name == name)
        .cloned()
        .ok_or_else(|| ApplicationError::UnknownName(name.to_owned()))?;
    if name == new_name {
        return lifecycle_result("registration-renamed", existing, &configuration.bin_dir);
    }
    let old_link = managed_link(&configuration.bin_dir, name)?;
    let new_link = managed_link(&configuration.bin_dir, new_name)?;
    if let Some(duplicate) = registry
        .registrations()
        .iter()
        .find(|registration| registration.name == new_name)
    {
        return Ok(LifecycleResult {
            status: LifecycleStatus::Blocked,
            identifier: "command-name-registered".to_owned(),
            registration: Some(inspect_registration(&existing, &configuration.bin_dir)?),
            conflict: Some(format!(
                "Command Name {new_name:?} is already registered to {}; choose another Command Name; {}",
                duplicate.target.display(),
                rename_paths_summary(&old_link, &new_link)
            )),
        });
    }
    let old_exists = fs::symlink_metadata(&old_link).is_ok();
    let old_owned = is_owned_link(&old_link, &existing.target);
    if existing.enabled && old_exists && !old_owned {
        return Ok(LifecycleResult {
            status: LifecycleStatus::Blocked,
            identifier: "rename-source-conflict".to_owned(),
            registration: Some(inspect_registration(&existing, &configuration.bin_dir)?),
            conflict: Some(format!(
                "old Managed Link ownership changed and it was left untouched; resolve it manually, then retry; {}",
                rename_paths_summary(&old_link, &new_link)
            )),
        });
    }
    match fs::symlink_metadata(&new_link) {
        Ok(_) => {
            return Ok(LifecycleResult {
                status: LifecycleStatus::Blocked,
                identifier: "rename-destination-conflict".to_owned(),
                registration: Some(inspect_registration(&existing, &configuration.bin_dir)?),
                conflict: Some(format!(
                    "new managed path {} is occupied; remove or rename that unmanaged entry, then retry; {}",
                    new_link.display(),
                    rename_paths_summary(&old_link, &new_link)
                )),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(ApplicationError::InspectManagedPath {
                path: new_link,
                source,
            })
        }
    }
    let mut updated = existing.clone();
    updated.name = new_name.to_owned();
    if existing.enabled {
        fs::create_dir_all(&configuration.bin_dir).map_err(|source| {
            ApplicationError::CreateManagedDirectory {
                path: configuration.bin_dir.clone(),
                source,
            }
        })?;
        faults
            .check(MutationBoundary::ManagedLinkCreation)
            .map_err(|source| ApplicationError::CreateManagedLink {
                path: new_link.clone(),
                source,
            })?;
        symlink(&existing.target, &new_link).map_err(|source| {
            ApplicationError::CreateManagedLink {
                path: new_link.clone(),
                source,
            }
        })?;
    }
    if old_owned {
        if let Err(error) = remove_owned_link(&old_link, &existing.target, faults) {
            if existing.enabled {
                let _ = remove_owned_link(&new_link, &existing.target, &NoMutationFaults);
            }
            return Err(error);
        }
    }
    let mut registrations = registry.registrations().to_vec();
    let position = registrations
        .iter()
        .position(|registration| registration.name == name)
        .expect("locked Registry still contains Registration");
    registrations[position] = updated.clone();
    registrations.sort_by(|left, right| left.name.cmp(&right.name));
    if let Err(error) = registry.replace(registrations, faults, git_sync, environment) {
        if !error.replacement_committed() {
            if existing.enabled {
                let _ = remove_owned_link(&new_link, &existing.target, &NoMutationFaults);
            }
            if old_owned && fs::symlink_metadata(&old_link).is_err() {
                let _ = symlink(&existing.target, &old_link);
            }
        }
        return Err(error.into());
    }
    lifecycle_result("registration-renamed", updated, &configuration.bin_dir)
}

pub fn path_diagnostic(environment: &Environment) -> Result<PathDiagnostic, ApplicationError> {
    let bin_dir = configuration::load(environment)?.bin_dir;
    let matches = environment
        .variable("PATH")
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|component| {
            let absolute = if component.is_absolute() {
                normalize(component)
            } else {
                normalize(&environment.cwd().join(component))
            };
            absolute == bin_dir
        })
        .count();
    let status = if matches == 0 {
        PathStatus::Missing
    } else {
        PathStatus::Present
    };
    Ok(PathDiagnostic {
        identifier: if matches == 0 {
            "managed-bin-not-on-path"
        } else {
            "managed-bin-on-path"
        }
        .to_owned(),
        status,
        bin_dir: bin_dir.clone(),
        matches,
        guidance: if matches == 0 {
            format!(
                "add {} as a complete PATH component in your shell configuration, then start a new shell",
                bin_dir.display()
            )
        } else {
            format!("managed bin directory {} is on PATH", bin_dir.display())
        },
    })
}

pub fn ignore_target(target: &Path, environment: &Environment) -> Result<String, ApplicationError> {
    ignore_target_with_git_sync(target, environment, &ConfigGitSync::blocking())
}

fn ignore_target_with_git_sync(
    target: &Path,
    environment: &Environment,
    git_sync: &ConfigGitSync,
) -> Result<String, ApplicationError> {
    let target = absolute_target(target, environment);
    discovery_ignore::add_with_git_sync(&target, environment, git_sync).map_err(|error| {
        ApplicationError::Operation {
            identifier: "ignore-failed",
            message: error.to_string(),
            resulting_registration: None,
        }
    })?;
    Ok("target-ignored".to_owned())
}

pub fn search(
    request: SearchRequest,
    environment: &Environment,
) -> Result<SearchResult, ApplicationError> {
    let configuration = configuration::load(environment)?;
    let root = request
        .search_root
        .unwrap_or_else(|| environment.cwd().to_path_buf());
    let root = if root.is_absolute() {
        normalize(&root)
    } else {
        normalize(&environment.cwd().join(root))
    };
    let registrations = registry::load(environment)?;
    let ignored_paths = discovery_ignore::load(environment)?;
    let found = if root == configuration.bin_dir {
        discovery::Discovery {
            targets: Vec::new(),
            warnings: Vec::new(),
        }
    } else {
        discovery::discover(
            &root,
            &configuration.bin_dir,
            &configuration.ignore,
            &ignored_paths,
        )
        .map_err(|source| ApplicationError::Search {
            path: root.clone(),
            source,
        })?
    };
    let mut targets_by_name = BTreeMap::<String, Vec<PathBuf>>::new();
    for target in &found.targets {
        if let Some(name) = target.file_name().and_then(|name| name.to_str()) {
            targets_by_name
                .entry(name.to_owned())
                .or_default()
                .push(target.clone());
        }
    }
    let mut candidates = Vec::new();
    for target in found.targets {
        let Some(proposed_name) = target
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
        else {
            continue;
        };
        let registration = registrations
            .iter()
            .find(|registration| registration.target == target)
            .cloned();
        let registration_state = registration
            .as_ref()
            .map(|registration| inspect_registration(registration, &configuration.bin_dir))
            .transpose()?;
        let conflict = registrations
            .iter()
            .find(|registration| {
                registration.name == proposed_name && registration.target != target
            })
            .map(|registration| NameConflict {
                kind: ConflictKind::CommandNameRegistered,
                conflicting_target: registration.target.clone(),
            });
        let conflict = if conflict.is_some() || registration.is_some() {
            conflict
        } else if targets_by_name
            .get(&proposed_name)
            .map(Vec::len)
            .unwrap_or(0)
            > 1
        {
            let conflicting_target = targets_by_name[&proposed_name]
                .iter()
                .find(|candidate_target| **candidate_target != target)
                .expect("duplicate proposed name has another Target")
                .clone();
            Some(NameConflict {
                kind: ConflictKind::DuplicateProposedName,
                conflicting_target,
            })
        } else {
            let managed_path = configuration.bin_dir.join(&proposed_name);
            match fs::symlink_metadata(&managed_path) {
                Ok(_) => Some(NameConflict {
                    kind: ConflictKind::ManagedPathOccupied,
                    conflicting_target: managed_path,
                }),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(source) => {
                    return Err(ApplicationError::InspectManagedPath {
                        path: managed_path,
                        source,
                    })
                }
            }
        };
        candidates.push(Candidate {
            proposed_name,
            target,
            registration: registration_state,
            conflict,
        });
    }
    let status = if candidates
        .iter()
        .any(|candidate| candidate.conflict.is_some())
    {
        SearchStatus::Blocked
    } else {
        SearchStatus::Healthy
    };
    Ok(SearchResult {
        status,
        search_root: root,
        candidates,
        warnings: found.warnings,
    })
}
