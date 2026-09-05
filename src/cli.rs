use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;

use crate::application::{
    add, disable, enable, ignore_target, list, path_diagnostic, remove, rename, search, AddRequest,
    ApplicationError, SearchRequest,
};
use crate::environment::Environment;
use crate::model::{
    LifecycleResult, LifecycleStatus, ListResult, PathDiagnostic, PathStatus, RegistrationState,
    SearchResult, SearchStatus,
};

#[derive(Parser)]
#[command(
    name = "bin",
    version,
    about = "Publish local targets under stable Command Names"
)]
struct Arguments {
    #[command(subcommand)]
    operation: Option<Operation>,
}

#[derive(Subcommand)]
enum Operation {
    /// Discover executable Targets without changing anything.
    Search {
        search_root: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// List all Registrations.
    List {
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Add a Registration.
    Add {
        target: PathBuf,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        disabled: bool,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Enable a Registration.
    Enable {
        name: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Disable a Registration.
    Disable {
        name: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Remove a Registration.
    Remove {
        name: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Change a Registration's Command Name.
    Rename {
        name: String,
        new_name: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Ignore a Target during discovery.
    Ignore {
        target: PathBuf,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

pub fn run() -> i32 {
    let arguments = match Arguments::try_parse() {
        Ok(arguments) => arguments,
        Err(error) => {
            let exit_code = error.exit_code();
            let _ = error.print();
            return exit_code;
        }
    };
    let environment = match Environment::current() {
        Ok(environment) => environment,
        Err(_) => {
            eprintln!("HOME is required to resolve configuration paths");
            return 2;
        }
    };
    let roots = crate::configuration::load(&environment)
        .map(|configuration| configuration.roots)
        .unwrap_or_default();
    let paths = PathDisplay {
        home: environment.home(),
        roots: &roots,
    };
    match arguments.operation {
        None => match crate::tui::run(None, &environment) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("error[tui-failed]: {error}");
                2
            }
        },
        Some(Operation::Search {
            search_root,
            format,
        }) => match search(SearchRequest { search_root }, &environment) {
            Ok(result) => {
                match format {
                    OutputFormat::Text => print_search(&result, &paths),
                    OutputFormat::Json => print_json(&result),
                }
                match result.status {
                    SearchStatus::Healthy => 0,
                    SearchStatus::Blocked => 1,
                }
            }
            Err(error) => print_error(format, "search-failed", error, &paths),
        },
        Some(Operation::List { format }) => match list(&environment) {
            Ok(result) => print_list_result(format, &result, &paths),
            Err(error) => print_error(format, "operation-failed", error, &paths),
        },
        Some(Operation::Add {
            target,
            name,
            disabled,
            format,
        }) => {
            match add(
                AddRequest {
                    target,
                    name,
                    disabled,
                },
                &environment,
            ) {
                Ok(result) => print_publishing_result(format, &result, &environment, &paths),
                Err(error) => print_error(format, "operation-failed", error, &paths),
            }
        }
        Some(Operation::Enable { name, format }) => match enable(&name, &environment) {
            Ok(result) => print_publishing_result(format, &result, &environment, &paths),
            Err(error) => print_error(format, "operation-failed", error, &paths),
        },
        Some(Operation::Disable { name, format }) => match disable(&name, &environment) {
            Ok(result) => print_lifecycle_result(format, &result, &paths),
            Err(error) => print_error(format, "operation-failed", error, &paths),
        },
        Some(Operation::Remove { name, format }) => match remove(&name, &environment) {
            Ok(result) => print_lifecycle_result(format, &result, &paths),
            Err(error) => print_error(format, "operation-failed", error, &paths),
        },
        Some(Operation::Rename {
            name,
            new_name,
            format,
        }) => match rename(&name, &new_name, &environment) {
            Ok(result) => print_publishing_result(format, &result, &environment, &paths),
            Err(error) => print_error(format, "operation-failed", error, &paths),
        },
        Some(Operation::Ignore { target, format }) => match ignore_target(&target, &environment) {
            Ok(identifier) => print_identifier_result(format, &identifier),
            Err(error) => print_error(format, "operation-failed", error, &paths),
        },
    }
}

fn print_publishing_result(
    format: OutputFormat,
    result: &LifecycleResult,
    environment: &Environment,
    paths: &PathDisplay<'_>,
) -> i32 {
    let published = result.status == LifecycleStatus::Healthy
        && result
            .registration
            .as_ref()
            .map(|state| state.registration.enabled)
            .unwrap_or(false);
    let diagnostic = published
        .then(|| path_diagnostic(environment).ok())
        .flatten();
    match format {
        OutputFormat::Json => {
            print_json(&PublishingResult {
                result,
                path_diagnostic: diagnostic,
            });
            status_code(&result.status)
        }
        OutputFormat::Text => {
            let code = print_lifecycle_result(format, result, paths);
            if let Some(diagnostic) = diagnostic.filter(|item| item.status == PathStatus::Missing) {
                eprintln!(
                    "warning[{}]: {}",
                    diagnostic.identifier, diagnostic.guidance
                );
            }
            code
        }
    }
}

fn print_lifecycle_result(
    format: OutputFormat,
    result: &LifecycleResult,
    paths: &PathDisplay<'_>,
) -> i32 {
    match format {
        OutputFormat::Json => print_json(result),
        OutputFormat::Text => {
            println!("Result: {}", result.identifier);
            if let Some(state) = &result.registration {
                print_registration(state, paths);
            }
            if let Some(conflict) = &result.conflict {
                println!("Conflict: {conflict}");
            }
        }
    }
    status_code(&result.status)
}

fn print_identifier_result(format: OutputFormat, identifier: &str) -> i32 {
    match format {
        OutputFormat::Json => print_json(&IdentifierResult {
            status: "healthy",
            identifier,
        }),
        OutputFormat::Text => println!("Result: {identifier}"),
    }
    0
}

fn print_list_result(format: OutputFormat, result: &ListResult, paths: &PathDisplay<'_>) -> i32 {
    match format {
        OutputFormat::Json => print_json(result),
        OutputFormat::Text => {
            println!("Result: {}", result.identifier);
            for state in &result.registrations {
                let mut annotations = Vec::new();
                if !state.registration.enabled {
                    annotations.push("disabled");
                }
                if let Some(defect) = &state.defect {
                    annotations.push(defect.kind.identifier());
                }

                let linked_item = format!(
                    "{} -> {}",
                    state.registration.name,
                    paths.path(&state.registration.target)
                );
                if annotations.is_empty() {
                    println!("{linked_item}");
                } else {
                    println!("{linked_item} [{}]", annotations.join("; "));
                }
            }
        }
    }
    status_code(&result.status)
}

fn status_code(status: &LifecycleStatus) -> i32 {
    match status {
        LifecycleStatus::Healthy => 0,
        LifecycleStatus::Blocked => 1,
    }
}

fn print_registration(state: &RegistrationState, paths: &PathDisplay<'_>) {
    println!("Registration: {}", state.registration.name);
    println!("Target: {}", paths.path(&state.registration.target));
    println!("Enabled: {}", state.registration.enabled);
    println!("Managed Link: {}", paths.path(&state.managed_link));
    println!("Actual: {}", actual_identifier(state));
    if let Some(target) = &state.observed_link_target {
        println!("Observed Link Target: {}", paths.path(target));
    }
    if let Some(defect) = &state.defect {
        println!("Defect: {}", defect.kind.identifier());
        println!("Reason: {}", defect.message);
    }
}

fn actual_identifier(state: &RegistrationState) -> &'static str {
    state.actual.identifier()
}

fn print_error(
    format: OutputFormat,
    kind: &'static str,
    error: ApplicationError,
    paths: &PathDisplay<'_>,
) -> i32 {
    let identifier = if kind == "operation-failed" {
        error.identifier()
    } else {
        kind
    };
    match format {
        OutputFormat::Text => {
            eprintln!("error[{identifier}]: {error}");
            if let Some(state) = error.resulting_registration() {
                if let Some(defect) = &state.defect {
                    eprintln!(
                        "Resulting Registration: {} -> {} [actual={}; defect={}]",
                        state.registration.name,
                        paths.path(&state.registration.target),
                        actual_identifier(state),
                        defect.kind.identifier()
                    );
                    eprintln!("Next safe action: {}", defect.message);
                } else {
                    eprintln!(
                        "Resulting Registration: {} -> {} [actual={}]",
                        state.registration.name,
                        paths.path(&state.registration.target),
                        actual_identifier(state)
                    );
                }
            }
        }
        OutputFormat::Json => print_json(&ErrorResult {
            status: "error",
            identifier,
            registration: error.resulting_registration().cloned(),
            error: ErrorDetail {
                kind: identifier,
                message: error.to_string(),
            },
        }),
    }
    2
}

fn print_json(value: &impl Serialize) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("result is serializable")
    );
}

#[derive(Serialize)]
struct IdentifierResult<'a> {
    status: &'static str,
    identifier: &'a str,
}

#[derive(Serialize)]
struct PublishingResult<'a> {
    #[serde(flatten)]
    result: &'a LifecycleResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    path_diagnostic: Option<PathDiagnostic>,
}

#[derive(Serialize)]
struct ErrorResult {
    status: &'static str,
    identifier: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    registration: Option<RegistrationState>,
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    kind: &'static str,
    message: String,
}

struct PathDisplay<'a> {
    home: &'a Path,
    roots: &'a BTreeMap<String, PathBuf>,
}

impl PathDisplay<'_> {
    fn path(&self, path: &Path) -> String {
        crate::model::display_path_with_roots(path, self.home, self.roots)
    }
}

fn print_search(result: &SearchResult, paths: &PathDisplay<'_>) {
    for candidate in &result.candidates {
        if candidate.registration.is_none() {
            println!("{}", paths.path(&candidate.target));
        }
    }
    for warning in &result.warnings {
        eprintln!(
            "warning[{}]: {}: {}",
            warning.kind.identifier(),
            paths.path(&warning.path),
            warning.message
        );
    }
}

pub fn display_path(path: &Path, home: &Path) -> String {
    crate::model::display_path(path, home)
}
