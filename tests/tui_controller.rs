use std::collections::BTreeMap;
use std::path::PathBuf;

use bintui::environment::Environment;
use bintui::model::{
    Candidate, LifecycleStatus, ListResult, ManagedPathKind, Registration, RegistrationDefect,
    RegistrationDefectKind, RegistrationState, SearchResult, SearchStatus, SearchWarning,
    WarningKind,
};
use bintui::tui::{initial_search_root, Controller, Event, OperationResult, Request, View};

fn search_result(root: &str, names: &[&str]) -> SearchResult {
    SearchResult {
        status: SearchStatus::Healthy,
        search_root: PathBuf::from(root),
        candidates: names
            .iter()
            .map(|name| Candidate {
                proposed_name: (*name).to_owned(),
                target: PathBuf::from(root).join(name),
                registration: None,
                conflict: None,
            })
            .collect(),
        warnings: Vec::new(),
    }
}

fn successfully_register_focused_candidate(controller: &mut Controller) {
    controller.handle(Event::Toggle);
    assert!(matches!(
        controller.take_request(),
        Some(Request::ValidateAdd { .. })
    ));
    controller.complete(OperationResult::Validation(Ok(
        bintui::model::LifecycleResult {
            status: LifecycleStatus::Healthy,
            identifier: "add-valid".to_owned(),
            registration: None,
            conflict: None,
        },
    )));
    assert!(matches!(
        controller.take_request(),
        Some(Request::Add { .. })
    ));
    controller.complete(OperationResult::Mutation(Ok(
        bintui::model::LifecycleResult {
            status: LifecycleStatus::Healthy,
            identifier: "registration-added".to_owned(),
            registration: None,
            conflict: None,
        },
    )));
    assert!(matches!(
        controller.take_request(),
        Some(Request::Search(_))
    ));
}

#[test]
fn absent_search_root_defaults_to_process_working_directory() {
    let environment = Environment::from_values(
        PathBuf::from("/home/user"),
        PathBuf::from("/working/project"),
        BTreeMap::new(),
    );
    assert_eq!(
        initial_search_root(None, &environment),
        PathBuf::from("/working/project")
    );
    assert_eq!(
        initial_search_root(Some(PathBuf::from("/explicit")), &environment),
        PathBuf::from("/explicit")
    );
}

#[test]
fn starts_with_automatic_discovery_at_explicit_root() {
    let mut controller = Controller::new(PathBuf::from("/project"));

    assert_eq!(
        controller.take_request(),
        Some(Request::Search(PathBuf::from("/project")))
    );
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["alpha"],
    ))));

    let state = controller.semantic_state(80, 24);
    assert_eq!(state.view, View::Discover);
    assert_eq!(state.search_root, PathBuf::from("/project"));
    assert_eq!(state.items[0].name, "alpha");
    assert_eq!(
        state.hints,
        " tab    space    / search    r rename    del remove    q quit"
    );
}

#[test]
fn initial_discovery_hides_registered_candidates() {
    let mut result = search_result("/project", &["alpha", "zulu"]);
    result.candidates[0].registration = Some(registration_state("alpha", "/project/alpha", None));
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();

    controller.complete(OperationResult::Search(Ok(result)));

    let state = controller.semantic_state(80, 24);
    assert_eq!(state.items.len(), 1);
    assert_eq!(state.items[0].target, PathBuf::from("/project/zulu"));
    assert!(!state.items[0].checked);
}

#[test]
fn newly_registered_candidate_stays_visible_for_the_session() {
    let mut initial = search_result("/project", &["alpha", "zulu"]);
    initial.candidates[0].registration = Some(registration_state("alpha", "/project/alpha", None));
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(initial)));

    controller.handle(Event::Toggle);
    controller.take_request();
    controller.complete(OperationResult::Validation(Ok(
        bintui::model::LifecycleResult {
            status: LifecycleStatus::Healthy,
            identifier: "add-valid".to_owned(),
            registration: None,
            conflict: None,
        },
    )));
    controller.take_request();
    controller.complete(OperationResult::Mutation(Ok(
        bintui::model::LifecycleResult {
            status: LifecycleStatus::Healthy,
            identifier: "registration-added".to_owned(),
            registration: None,
            conflict: None,
        },
    )));
    controller.take_request();
    let mut reloaded = search_result("/project", &["alpha", "zulu"]);
    reloaded.candidates[0].registration = Some(registration_state("alpha", "/project/alpha", None));
    reloaded.candidates[1].registration = Some(registration_state("zulu", "/project/zulu", None));
    controller.complete(OperationResult::Search(Ok(reloaded)));

    let state = controller.semantic_state(80, 24);
    assert_eq!(state.items.len(), 1);
    assert_eq!(state.items[0].target, PathBuf::from("/project/zulu"));
    assert!(state.items[0].checked);
}

#[test]
fn delete_filter_reload_keeps_the_remaining_discovery_order() {
    let initial = search_result("/project", &["alpha", "beta", "zulu"]);
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(initial)));

    controller.handle(Event::Down);
    controller.handle(Event::Delete);
    assert_eq!(
        controller.take_request(),
        Some(Request::Ignore(PathBuf::from("/project/beta")))
    );
    controller.complete(OperationResult::Ignore(Ok("target-ignored".to_owned())));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["alpha", "zulu"],
    ))));

    let targets: Vec<_> = controller
        .semantic_state(80, 24)
        .items
        .iter()
        .map(|item| item.target.clone())
        .collect();
    assert_eq!(
        targets,
        vec![
            PathBuf::from("/project/alpha"),
            PathBuf::from("/project/zulu")
        ]
    );
}

#[test]
fn delete_ignores_the_focused_discovery_target_and_reloads_the_list() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["alpha", "beta"],
    ))));
    controller.handle(Event::Down);

    controller.handle(Event::Delete);

    assert_eq!(
        controller.take_request(),
        Some(Request::Ignore(PathBuf::from("/project/beta")))
    );
    assert!(controller.has_emitted_mutation());
    controller.complete(OperationResult::Ignore(Ok("target-ignored".to_owned())));
    assert_eq!(
        controller.take_request(),
        Some(Request::Search(PathBuf::from("/project")))
    );
    assert_eq!(
        controller.semantic_state(80, 24).notice.as_deref(),
        Some("target-ignored")
    );
}

#[test]
fn filter_matches_names_and_paths_without_changing_case() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["Alpha", "beta", "gamma-tool"],
    ))));

    controller.handle(Event::StartFilter);
    controller.handle(Event::Text("ALP".to_owned()));

    let state = controller.semantic_state(80, 24);
    assert!(state.filter_active);
    assert_eq!(state.filter, "ALP");
    assert_eq!(state.items.len(), 1);
    assert_eq!(state.items[0].name, "Alpha");

    controller.handle(Event::ClearFilter);
    let state = controller.semantic_state(80, 24);
    assert!(!state.filter_active);
    assert!(state.filter.is_empty());
    assert_eq!(state.items.len(), 3);
}

#[test]
fn filter_navigation_and_actions_use_only_visible_items() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["alpha", "beta", "beta-helper"],
    ))));

    controller.handle(Event::StartFilter);
    controller.handle(Event::Text("beta".to_owned()));
    controller.handle(Event::Down);
    controller.handle(Event::Submit);
    controller.handle(Event::Delete);

    assert_eq!(
        controller.take_request(),
        Some(Request::Ignore(PathBuf::from("/project/beta-helper")))
    );
}

#[test]
fn escape_clears_a_retained_filter_after_input_is_submitted() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["alpha", "beta"],
    ))));

    controller.handle(Event::StartFilter);
    controller.handle(Event::Text("alpha".to_owned()));
    controller.handle(Event::Submit);
    assert_eq!(controller.semantic_state(80, 24).items.len(), 1);

    controller.handle(Event::Dismiss);
    let state = controller.semantic_state(80, 24);
    assert!(state.filter.is_empty());
    assert_eq!(state.items.len(), 2);
}

#[test]
fn navigation_and_view_changes_do_not_emit_mutations() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["alpha", "beta"],
    ))));

    controller.handle(Event::Down);
    controller.handle(Event::Up);
    controller.handle(Event::SwitchView);

    assert_eq!(controller.view(), View::Registrations);
    assert_eq!(controller.take_request(), Some(Request::List));
    assert!(!controller.has_emitted_mutation());
}

#[test]
fn checkbox_emits_one_request_and_reloads_after_failure() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["alpha"],
    ))));

    controller.handle(Event::Toggle);
    let add_request = Request::Add {
        target: PathBuf::from("/project/alpha"),
        name: "alpha".to_owned(),
    };
    assert_eq!(
        controller.take_request(),
        Some(Request::ValidateAdd {
            target: PathBuf::from("/project/alpha"),
            name: "alpha".to_owned(),
        })
    );
    controller.handle(Event::Toggle);
    assert_eq!(controller.take_request(), None);
    controller.complete(OperationResult::Validation(Ok(
        bintui::model::LifecycleResult {
            status: LifecycleStatus::Healthy,
            identifier: "add-valid".to_owned(),
            registration: None,
            conflict: None,
        },
    )));
    assert_eq!(controller.take_request(), Some(add_request));

    controller.complete(OperationResult::Mutation(Err(
        bintui::tui::OperationError::new("add-failed", "permission denied"),
    )));
    assert_eq!(
        controller.take_request(),
        Some(Request::Search(PathBuf::from("/project")))
    );
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["alpha"],
    ))));
    assert!(!controller.semantic_state(80, 24).items[0].checked);
}

#[test]
fn successful_checkbox_uses_reloaded_registration_state() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["alpha"],
    ))));
    controller.handle(Event::Toggle);
    controller.take_request();
    controller.complete(OperationResult::Validation(Ok(
        bintui::model::LifecycleResult {
            status: LifecycleStatus::Healthy,
            identifier: "add-valid".to_owned(),
            registration: None,
            conflict: None,
        },
    )));
    controller.take_request();
    controller.complete(OperationResult::Mutation(Ok(
        bintui::model::LifecycleResult {
            status: LifecycleStatus::Healthy,
            identifier: "registration-added".to_owned(),
            registration: None,
            conflict: None,
        },
    )));
    controller.take_request();
    let mut reloaded = search_result("/project", &["alpha"]);
    reloaded.candidates[0].registration = Some(registration_state("alpha", "/project/alpha", None));
    controller.complete(OperationResult::Search(Ok(reloaded)));

    let state = controller.semantic_state(80, 24);
    assert!(state.items[0].checked);
    assert_eq!(state.notice.as_deref(), Some("registration-added"));
}

#[test]
fn discover_reload_exposes_actual_registration_defect_after_mutation() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["alpha"],
    ))));
    controller.handle(Event::Toggle);
    controller.take_request();
    controller.complete(OperationResult::Validation(Ok(
        bintui::model::LifecycleResult {
            status: LifecycleStatus::Healthy,
            identifier: "add-valid".to_owned(),
            registration: None,
            conflict: None,
        },
    )));
    controller.take_request();
    controller.complete(OperationResult::Mutation(Ok(
        bintui::model::LifecycleResult {
            status: LifecycleStatus::Healthy,
            identifier: "registration-added".to_owned(),
            registration: None,
            conflict: None,
        },
    )));
    controller.take_request();
    let mut reloaded = search_result("/project", &["alpha"]);
    let registration = Registration {
        name: "alpha".to_owned(),
        target: PathBuf::from("/project/alpha"),
        enabled: true,
    };
    reloaded.candidates[0].registration = Some(RegistrationState {
        registration,
        managed_link: PathBuf::from("/managed/alpha"),
        actual: ManagedPathKind::File,
        observed_link_target: None,
        defect: Some(RegistrationDefect {
            kind: RegistrationDefectKind::Conflict,
            message: "managed path contains an unmanaged file".to_owned(),
        }),
    });
    controller.complete(OperationResult::Search(Ok(reloaded)));

    let item = &controller.semantic_state(80, 24).items[0];
    assert!(item.checked);
    assert_eq!(item.status, "conflict file");
}

#[test]
fn mutation_error_stays_attached_to_affected_item_during_navigation() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["alpha", "beta"],
    ))));
    controller.handle(Event::Toggle);
    controller.take_request();
    controller.complete(OperationResult::Validation(Ok(
        bintui::model::LifecycleResult {
            status: LifecycleStatus::Healthy,
            identifier: "add-valid".to_owned(),
            registration: None,
            conflict: None,
        },
    )));
    controller.take_request();
    controller.handle(Event::Down);
    controller.complete(OperationResult::Mutation(Err(
        bintui::tui::OperationError {
            identifier: "add-failed".to_owned(),
            message: "permission denied".to_owned(),
            registration_name: Some("alpha".to_owned()),
        },
    )));

    let items = controller.semantic_state(80, 24).items;
    assert!(items[0].error.is_some());
    assert!(items[1].error.is_none());
}

#[test]
fn discovery_warnings_are_visible_and_refresh_with_search_results() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    let mut warned = search_result("/project", &["alpha"]);
    warned.warnings.push(SearchWarning {
        kind: WarningKind::UnreadableDirectory,
        path: PathBuf::from("/project/private"),
        message: "permission denied".to_owned(),
    });

    controller.complete(OperationResult::Search(Ok(warned)));

    assert!(controller
        .semantic_state(80, 24)
        .notice
        .as_deref()
        .unwrap()
        .contains("warning[unreadable-directory]"));

    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["alpha"],
    ))));
    assert_eq!(controller.semantic_state(80, 24).notice, None);
}

#[test]
fn mutation_time_conflict_is_shown_and_never_emits_add() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["alpha"],
    ))));

    controller.handle(Event::Toggle);
    controller.take_request();
    controller.complete(OperationResult::Validation(Ok(
        bintui::model::LifecycleResult {
            status: LifecycleStatus::Blocked,
            identifier: "managed-path-occupied".to_owned(),
            registration: None,
            conflict: Some("managed path is occupied".to_owned()),
        },
    )));

    assert_eq!(
        controller.take_request(),
        Some(Request::Search(PathBuf::from("/project")))
    );
    assert!(!controller.has_emitted_mutation());
    assert!(controller.semantic_state(80, 24).items[0]
        .error
        .as_deref()
        .unwrap()
        .contains("managed-path-occupied"));
}

#[test]
fn candidate_rename_cancel_and_submit_are_local_until_toggle() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["alpha"],
    ))));

    controller.handle(Event::Rename);
    controller.handle(Event::Text("-new".to_owned()));
    controller.handle(Event::Cancel);
    assert_eq!(controller.semantic_state(80, 24).items[0].name, "alpha");

    controller.handle(Event::Rename);
    controller.handle(Event::Backspace);
    controller.handle(Event::Text("z".to_owned()));
    controller.handle(Event::Submit);
    assert_eq!(controller.semantic_state(80, 24).items[0].name, "alphz");
    assert_eq!(controller.take_request(), None);
}

#[test]
fn newly_registered_discovery_keeps_its_session_name_after_reload() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["tool"],
    ))));
    successfully_register_focused_candidate(&mut controller);
    let mut result = search_result("/project", &["tool"]);
    result.candidates[0].registration = Some(registration_state("alias", "/project/tool", None));

    controller.complete(OperationResult::Search(Ok(result)));

    assert_eq!(controller.semantic_state(80, 24).items[0].name, "tool");
}

#[test]
fn newly_registered_discovery_checkbox_emits_remove_not_add() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["alpha"],
    ))));
    successfully_register_focused_candidate(&mut controller);
    let mut result = search_result("/project", &["alpha"]);
    result.candidates[0].registration = Some(registration_state("alpha", "/project/alpha", None));
    controller.complete(OperationResult::Search(Ok(result)));

    assert!(!controller.semantic_state(80, 24).hints.contains("inspect"));
    controller.handle(Event::Toggle);
    assert_eq!(
        controller.take_request(),
        Some(Request::Remove("alpha".to_owned()))
    );
}

#[test]
fn failed_remove_keeps_checkbox_checked_after_reload() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["alpha"],
    ))));
    successfully_register_focused_candidate(&mut controller);
    let mut registered = search_result("/project", &["alpha"]);
    registered.candidates[0].registration =
        Some(registration_state("alpha", "/project/alpha", None));
    controller.complete(OperationResult::Search(Ok(registered.clone())));
    controller.handle(Event::Toggle);
    controller.take_request();
    controller.complete(OperationResult::Mutation(Err(
        bintui::tui::OperationError::new("remove-failed", "managed path changed"),
    )));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(registered)));

    let item = &controller.semantic_state(80, 24).items[0];
    assert!(item.checked);
    assert!(item.error.as_deref().unwrap().contains("remove-failed"));
}

#[test]
fn registry_items_with_defects_are_listed_first() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.handle(Event::SwitchView);
    controller.take_request();
    controller.complete(OperationResult::List(Ok(ListResult {
        status: LifecycleStatus::Blocked,
        identifier: "registrations-listed".to_owned(),
        registrations: vec![
            registration_state("healthy-alpha", "/outside/healthy-alpha", None),
            registration_state(
                "issue-bravo",
                "/outside/issue-bravo",
                Some(RegistrationDefectKind::LinkMissing),
            ),
            registration_state(
                "issue-charlie",
                "/outside/issue-charlie",
                Some(RegistrationDefectKind::TargetMissing),
            ),
            registration_state("healthy-delta", "/outside/healthy-delta", None),
        ],
    })));

    let items = controller.semantic_state(80, 24).items;
    assert_eq!(
        items
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "issue-bravo",
            "issue-charlie",
            "healthy-alpha",
            "healthy-delta"
        ]
    );
}

#[test]
fn registry_items_are_sorted_by_issue_root_and_command_name() {
    let roots = BTreeMap::from([
        ("alpha".to_owned(), PathBuf::from("/workspace/alpha")),
        ("beta".to_owned(), PathBuf::from("/workspace/beta")),
    ]);
    let mut controller = Controller::with_roots(
        PathBuf::from("/workspace"),
        PathBuf::from("/home/test"),
        roots,
    );
    controller.take_request();
    controller.handle(Event::SwitchView);
    controller.take_request();
    controller.complete(OperationResult::List(Ok(ListResult {
        status: LifecycleStatus::Blocked,
        identifier: "registrations-listed".to_owned(),
        registrations: vec![
            registration_state("healthy-zulu", "/workspace/beta/tool-a", None),
            registration_state(
                "issue-zulu",
                "/workspace/beta/tool-b",
                Some(RegistrationDefectKind::LinkMissing),
            ),
            registration_state("healthy-bravo", "/workspace/alpha/tool-z", None),
            registration_state(
                "issue-bravo",
                "/workspace/beta/tool-z",
                Some(RegistrationDefectKind::TargetMissing),
            ),
            registration_state(
                "issue-zulu-alpha",
                "/workspace/alpha/tool-a",
                Some(RegistrationDefectKind::LinkMissing),
            ),
            registration_state("healthy-alpha", "/workspace/beta/tool-z", None),
        ],
    })));

    assert_eq!(
        controller
            .semantic_state(80, 24)
            .items
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "issue-zulu-alpha",
            "issue-bravo",
            "issue-zulu",
            "healthy-bravo",
            "healthy-alpha",
            "healthy-zulu",
        ]
    );
}

#[test]
fn registration_checkboxes_show_and_change_enabled_state() {
    let mut enabled = registration_controller(registration_state("alpha", "/outside/alpha", None));

    let state = enabled.semantic_state(80, 24);
    assert!(state.items[0].checked);
    assert_eq!(state.items[0].target, PathBuf::from("/outside/alpha"));
    assert_eq!(
        state.hints,
        " tab    space    / search    r rename    del remove    q quit"
    );

    enabled.handle(Event::Toggle);
    assert_eq!(
        enabled.take_request(),
        Some(Request::Disable("alpha".to_owned()))
    );

    let mut disabled_state = registration_state("alpha", "/outside/alpha", None);
    disabled_state.registration.enabled = false;
    let mut disabled = registration_controller(disabled_state);
    assert!(!disabled.semantic_state(80, 24).items[0].checked);

    disabled.handle(Event::Toggle);
    assert_eq!(
        disabled.take_request(),
        Some(Request::Enable("alpha".to_owned()))
    );
}

#[test]
fn delete_disables_the_focused_enabled_registration() {
    let mut controller =
        registration_controller(registration_state("alpha", "/project/alpha", None));

    controller.handle(Event::Delete);

    assert_eq!(
        controller.take_request(),
        Some(Request::Disable("alpha".to_owned()))
    );
    assert!(controller
        .semantic_state(80, 24)
        .hints
        .contains("del remove"));
}

#[test]
fn disabling_a_broken_registration_keeps_it_in_place_and_unchecked() {
    let broken = registration_state(
        "zulu",
        "/project/zulu",
        Some(RegistrationDefectKind::LinkBroken),
    );
    let healthy = registration_state("alpha", "/project/alpha", None);
    let mut controller = registration_controller(broken.clone());
    controller.complete(OperationResult::List(Ok(ListResult {
        status: LifecycleStatus::Blocked,
        identifier: "registrations-listed".to_owned(),
        registrations: vec![healthy.clone(), broken.clone()],
    })));
    controller.handle(Event::Delete);
    assert_eq!(
        controller.take_request(),
        Some(Request::Disable("zulu".to_owned()))
    );
    let mut disabled = broken;
    disabled.registration.enabled = false;
    disabled.actual = ManagedPathKind::Missing;
    disabled.defect = None;
    controller.complete(OperationResult::Mutation(Ok(
        bintui::model::LifecycleResult {
            status: LifecycleStatus::Healthy,
            identifier: "registration-disabled".to_owned(),
            registration: Some(disabled.clone()),
            conflict: None,
        },
    )));
    assert_eq!(controller.take_request(), Some(Request::List));
    controller.complete(OperationResult::List(Ok(ListResult {
        status: LifecycleStatus::Healthy,
        identifier: "registrations-listed".to_owned(),
        registrations: vec![healthy, disabled],
    })));
    let state = controller.semantic_state(80, 24);
    assert_eq!(state.items.len(), 2);
    assert_eq!(state.items[0].name, "zulu");
    assert!(!state.items[0].checked);
    controller.handle(Event::Delete);
    assert_eq!(
        controller.take_request(),
        Some(Request::Remove("zulu".to_owned()))
    );
}

#[test]
fn reopening_registrations_restores_error_first_sort_order() {
    let mut controller = registration_controller(registration_state("zulu", "/project/zulu", None));
    controller.handle(Event::SwitchView);
    controller.take_request();
    controller.handle(Event::SwitchView);
    assert_eq!(controller.take_request(), Some(Request::List));
    controller.complete(OperationResult::List(Ok(ListResult {
        status: LifecycleStatus::Blocked,
        identifier: "registrations-listed".to_owned(),
        registrations: vec![
            registration_state("zulu", "/project/zulu", None),
            registration_state("alpha", "/project/alpha", None),
            registration_state(
                "issue",
                "/project/issue",
                Some(RegistrationDefectKind::TargetMissing),
            ),
        ],
    })));
    let state = controller.semantic_state(80, 24);
    assert_eq!(
        state
            .items
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        vec!["issue", "alpha", "zulu"]
    );
}

#[test]
fn delete_unregisters_the_focused_disabled_registration_immediately() {
    let mut state = registration_state("alpha", "/project/alpha", None);
    state.registration.enabled = false;
    let mut controller = registration_controller(state);

    controller.handle(Event::Delete);

    assert_eq!(
        controller.take_request(),
        Some(Request::Remove("alpha".to_owned()))
    );
    assert!(controller.semantic_state(80, 24).dialog.is_none());
}

#[test]
fn exit_and_error_dismissal_are_explicit_state_transitions() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Err(
        bintui::tui::OperationError::new("search-failed", "not readable"),
    )));
    assert!(controller.semantic_state(80, 24).dialog.is_some());
    controller.handle(Event::Cancel);
    assert!(controller.semantic_state(80, 24).dialog.is_none());
    controller.handle(Event::Exit);
    assert!(controller.should_exit());
}

fn registration_state(
    name: &str,
    target: &str,
    defect: Option<RegistrationDefectKind>,
) -> RegistrationState {
    RegistrationState {
        registration: Registration {
            name: name.to_owned(),
            target: PathBuf::from(target),
            enabled: true,
        },
        managed_link: PathBuf::from("/managed").join(name),
        actual: if defect.is_none() {
            ManagedPathKind::OwnedLink
        } else {
            ManagedPathKind::Missing
        },
        observed_link_target: None,
        defect: defect.map(|kind| RegistrationDefect {
            kind,
            message: "inspect actual state".to_owned(),
        }),
    }
}

fn registration_controller(state: RegistrationState) -> Controller {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.handle(Event::SwitchView);
    controller.take_request();
    controller.complete(OperationResult::List(Ok(ListResult {
        status: LifecycleStatus::Healthy,
        identifier: "registrations-listed".to_owned(),
        registrations: vec![state],
    })));
    controller
}

#[test]
fn rename_is_available_for_registered_items_in_both_views() {
    let mut controller = Controller::new(PathBuf::from("/project"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(search_result(
        "/project",
        &["registered", "unregistered"],
    ))));
    successfully_register_focused_candidate(&mut controller);
    let mut discovery = search_result("/project", &["registered", "unregistered"]);
    discovery.candidates[0].registration = Some(registration_state(
        "registered",
        "/project/registered",
        None,
    ));
    controller.complete(OperationResult::Search(Ok(discovery)));

    controller.handle(Event::Rename);
    assert_eq!(
        controller.semantic_state(80, 24).dialog.as_deref(),
        Some("Command Name: registered")
    );
    controller.handle(Event::Cancel);

    controller.handle(Event::Down);
    controller.handle(Event::Rename);
    assert_eq!(
        controller.semantic_state(80, 24).dialog.as_deref(),
        Some("Edit Command Name (unregistered)\nunregistered")
    );
    controller.handle(Event::Cancel);

    let mut registrations =
        registration_controller(registration_state("registered", "/project/tool", None));
    registrations.handle(Event::Rename);
    assert_eq!(
        registrations.semantic_state(80, 24).dialog.as_deref(),
        Some("Command Name: registered")
    );
}

#[test]
fn registration_rename_dialog_submits_one_mutation_and_reloads_actual_state() {
    let mut controller = registration_controller(registration_state("old", "/project/tool", None));

    controller.handle(Event::Rename);
    assert_eq!(
        controller.semantic_state(80, 24).dialog.as_deref(),
        Some("Command Name: old")
    );
    for _ in 0..3 {
        controller.handle(Event::Backspace);
    }
    controller.handle(Event::Text("new".to_owned()));
    controller.handle(Event::Submit);
    assert_eq!(
        controller.take_request(),
        Some(Request::Rename {
            name: "old".to_owned(),
            new_name: "new".to_owned(),
        })
    );
    controller.complete(OperationResult::Mutation(Ok(
        bintui::model::LifecycleResult {
            status: LifecycleStatus::Healthy,
            identifier: "registration-renamed".to_owned(),
            registration: None,
            conflict: None,
        },
    )));
    assert_eq!(controller.take_request(), Some(Request::List));
}

#[test]
fn resize_and_empty_discovery_have_legible_semantics() {
    let mut controller = Controller::new(PathBuf::from("/empty"));
    controller.take_request();
    controller.complete(OperationResult::Search(Ok(search_result("/empty", &[]))));
    controller.handle(Event::Resize(30, 6));

    let state = controller.semantic_state(30, 6);
    assert!(state.compact);
    assert_eq!(
        state.empty_message.as_deref(),
        Some("No executable candidates found")
    );
    assert!(state.hints.contains("q quit"));
}
