//! Ratatui presentation adapter over the shared application layer.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Stdout};
use std::path::PathBuf;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, KeyCode, KeyEventKind, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    block::Padding, Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap,
};
use ratatui::{Frame, Terminal};

use crate::application::{self, AddRequest, ApplicationError, SearchRequest};
use crate::environment::Environment;
use crate::model::{
    Candidate, LifecycleResult, ListResult, ManagedPathKind, PathStatus, RegistrationState,
    SearchResult,
};
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Discover,
    Registrations,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Search(PathBuf),
    List,
    ValidateAdd { target: PathBuf, name: String },
    Add { target: PathBuf, name: String },
    Remove(String),
    Enable(String),
    Disable(String),
    Rename { name: String, new_name: String },
    Ignore(PathBuf),
}

impl Request {
    fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::Add { .. }
                | Self::Remove(_)
                | Self::Enable(_)
                | Self::Disable(_)
                | Self::Rename { .. }
                | Self::Ignore(_)
        )
    }
}

#[derive(Clone, Debug)]
pub struct OperationError {
    pub identifier: String,
    pub message: String,
    pub registration_name: Option<String>,
}

impl OperationError {
    pub fn new(identifier: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            identifier: identifier.into(),
            message: message.into(),
            registration_name: None,
        }
    }

    fn for_registration(mut self, name: impl Into<String>) -> Self {
        self.registration_name = Some(name.into());
        self
    }
}

impl From<ApplicationError> for OperationError {
    fn from(error: ApplicationError) -> Self {
        let identifier = error.identifier();
        let mut message = error.to_string();
        let registration_name = error
            .resulting_registration()
            .map(|state| state.registration.name.clone());
        if let Some(state) = error.resulting_registration() {
            if let Some(defect) = &state.defect {
                message.push_str(&format!(
                    "; Registration {} actual={} defect={}; next safe action: {}",
                    state.registration.name,
                    state.actual.identifier(),
                    defect.kind.identifier(),
                    defect.message
                ));
            }
        }
        let operation_error = Self::new(identifier, message);
        match registration_name {
            Some(name) => operation_error.for_registration(name),
            None => operation_error,
        }
    }
}

pub enum OperationResult {
    Search(Result<SearchResult, OperationError>),
    List(Result<ListResult, OperationError>),
    Validation(Result<LifecycleResult, OperationError>),
    Mutation(Result<LifecycleResult, OperationError>),
    Ignore(Result<String, OperationError>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Up,
    Down,
    SwitchView,
    Toggle,
    Delete,
    Rename,
    Submit,
    Cancel,
    Dismiss,
    StartFilter,
    ClearFilter,
    Text(String),
    Backspace,
    Resize(u16, u16),
    Mouse(MouseEvent),
    Exit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticItem {
    pub name: String,
    pub target: PathBuf,
    pub checked: bool,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticState {
    pub view: View,
    pub search_root: PathBuf,
    pub items: Vec<SemanticItem>,
    pub focused: usize,
    pub hints: String,
    pub filter: String,
    pub filter_active: bool,
    pub empty_message: Option<String>,
    pub compact: bool,
    pub dialog: Option<String>,
    pub notice: Option<String>,
}

#[derive(Clone, Debug)]
enum Dialog {
    EditName { original: String, value: String },
    Rename { name: String, value: String },
    Error { text: String },
}

const FOOTER_HINTS: &str = " tab    space    / search    r rename    del remove    q quit";

#[derive(Clone, Debug)]
struct ListEntry {
    name: String,
    target: PathBuf,
    registration: Option<RegistrationState>,
    unavailable: bool,
}

impl ListEntry {
    fn from_candidate(candidate: Candidate, name: String) -> Self {
        Self {
            target: candidate.target,
            registration: candidate.registration,
            unavailable: candidate.conflict.is_some(),
            name,
        }
    }

    fn from_registration(registration: RegistrationState) -> Self {
        Self {
            name: registration.registration.name.clone(),
            target: registration.registration.target.clone(),
            registration: Some(registration),
            unavailable: false,
        }
    }
}

fn registration_root_sort_key(
    target: &std::path::Path,
    home: &std::path::Path,
    roots: &BTreeMap<String, PathBuf>,
) -> String {
    let displayed = crate::model::display_path_with_roots(target, home, roots);
    displayed
        .strip_prefix('[')
        .and_then(|root| root.split_once(']'))
        .map_or(displayed.clone(), |(root, _)| format!("[{root}]"))
}

fn entry_matches_filter(entry: &ListEntry, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let filter = filter.to_lowercase();
    entry.name.to_lowercase().contains(&filter)
        || entry
            .target
            .to_string_lossy()
            .to_lowercase()
            .contains(&filter)
}

#[derive(Debug, Default)]
struct ItemList {
    entries: Vec<ListEntry>,
    focus: usize,
    state: ListState,
}

impl ItemList {
    fn focused(&self, filter: &str) -> Option<&ListEntry> {
        self.entries
            .iter()
            .filter(|entry| entry_matches_filter(entry, filter))
            .nth(self.focus)
    }

    fn replace(&mut self, entries: Vec<ListEntry>, filter: &str) {
        self.entries = entries;
        self.clamp_focus(filter);
    }

    fn move_focus(&mut self, delta: isize, filter: &str) {
        let visible_len = self.visible_len(filter);
        if visible_len > 0 {
            self.focus = ((self.focus as isize + delta).rem_euclid(visible_len as isize)) as usize;
        }
    }

    fn clamp_focus(&mut self, filter: &str) {
        self.focus = clamp_focus(self.focus, self.visible_len(filter));
    }

    fn visible_len(&self, filter: &str) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry_matches_filter(entry, filter))
            .count()
    }
}

#[derive(Default)]
struct MouseRegions {
    tabs: [Rect; 2],
    search: Rect,
    list: Rect,
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
}

pub struct Controller {
    view: View,
    search_root: PathBuf,
    home: PathBuf,
    roots: BTreeMap<String, PathBuf>,
    discover: ItemList,
    registrations: ItemList,
    dialog: Option<Dialog>,
    pending: Option<Request>,
    mutation_active: bool,
    pending_add: Option<(PathBuf, String)>,
    active_add_target: Option<PathBuf>,
    session_registered_targets: BTreeSet<PathBuf>,
    active_item_key: Option<String>,
    emitted_mutation: bool,
    exit: bool,
    size: (u16, u16),
    item_errors: std::collections::BTreeMap<String, String>,
    notice: Option<String>,
    path_warning: Option<String>,
    discovery_warning: Option<String>,
    filter: String,
    filter_active: bool,
    mouse_regions: MouseRegions,
}

impl Controller {
    pub fn new(search_root: PathBuf) -> Self {
        Self::with_paths(search_root, PathBuf::new(), BTreeMap::new())
    }

    pub fn with_roots(
        search_root: PathBuf,
        home: PathBuf,
        roots: BTreeMap<String, PathBuf>,
    ) -> Self {
        Self::with_paths(search_root, home, roots)
    }

    fn with_paths(search_root: PathBuf, home: PathBuf, roots: BTreeMap<String, PathBuf>) -> Self {
        Self {
            view: View::Discover,
            pending: Some(Request::Search(search_root.clone())),
            search_root,
            home,
            roots,
            discover: ItemList::default(),
            registrations: ItemList::default(),
            dialog: None,
            mutation_active: false,
            pending_add: None,
            active_add_target: None,
            session_registered_targets: BTreeSet::new(),
            active_item_key: None,
            emitted_mutation: false,
            exit: false,
            size: (80, 24),
            item_errors: Default::default(),
            notice: None,
            path_warning: None,
            discovery_warning: None,
            filter: String::new(),
            filter_active: false,
            mouse_regions: MouseRegions::default(),
        }
    }

    pub fn view(&self) -> View {
        self.view
    }
    pub fn should_exit(&self) -> bool {
        self.exit
    }
    pub fn has_emitted_mutation(&self) -> bool {
        self.emitted_mutation
    }
    pub fn take_request(&mut self) -> Option<Request> {
        self.pending.take()
    }

    pub fn complete(&mut self, result: OperationResult) {
        match result {
            OperationResult::Search(Ok(mut result)) => {
                self.discovery_warning = self.format_search_warnings(&result.warnings);
                self.search_root = result.search_root;
                let edited_names: std::collections::BTreeMap<_, _> = self
                    .discover
                    .entries
                    .iter()
                    .filter(|entry| entry.registration.is_none())
                    .map(|entry| (entry.target.clone(), entry.name.clone()))
                    .collect();
                let previous_order: std::collections::BTreeMap<_, _> = self
                    .discover
                    .entries
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| (entry.target.clone(), index))
                    .collect();
                result.candidates.retain(|candidate| {
                    candidate.registration.is_none()
                        || self.session_registered_targets.contains(&candidate.target)
                });
                result.candidates.sort_by(|left, right| {
                    match (
                        previous_order.get(&left.target),
                        previous_order.get(&right.target),
                    ) {
                        (Some(left), Some(right)) => left.cmp(right),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => left
                            .registration
                            .is_some()
                            .cmp(&right.registration.is_some())
                            .then_with(|| left.target.cmp(&right.target)),
                    }
                });
                let entries = result
                    .candidates
                    .into_iter()
                    .map(|candidate| {
                        let name =
                            edited_names
                                .get(&candidate.target)
                                .cloned()
                                .unwrap_or_else(|| {
                                    candidate
                                        .registration
                                        .as_ref()
                                        .map(|state| state.registration.name.clone())
                                        .unwrap_or_else(|| candidate.proposed_name.clone())
                                });
                        ListEntry::from_candidate(candidate, name)
                    })
                    .collect();
                self.discover.replace(entries, &self.filter);
            }
            OperationResult::Search(Err(error)) | OperationResult::List(Err(error)) => {
                self.dialog = Some(Dialog::Error {
                    text: format!("{}: {}", error.identifier, error.message),
                });
            }
            OperationResult::List(Ok(mut result)) => {
                result.registrations.sort_by_cached_key(|registration| {
                    (
                        registration.defect.is_none(),
                        registration_root_sort_key(
                            &registration.registration.target,
                            &self.home,
                            &self.roots,
                        ),
                        registration.registration.name.clone(),
                    )
                });
                self.registrations.replace(
                    result
                        .registrations
                        .into_iter()
                        .map(ListEntry::from_registration)
                        .collect(),
                    &self.filter,
                );
            }
            OperationResult::Validation(Ok(result))
                if result.status == crate::model::LifecycleStatus::Healthy =>
            {
                if let Some((target, name)) = self.pending_add.take() {
                    self.emitted_mutation = true;
                    self.active_add_target = Some(target.clone());
                    self.pending = Some(Request::Add { target, name });
                }
            }
            OperationResult::Validation(result) => {
                self.mutation_active = false;
                self.pending_add = None;
                self.active_add_target = None;
                let error = match result {
                    Ok(result) => OperationError::new(
                        result.identifier,
                        result
                            .conflict
                            .unwrap_or_else(|| "Registration is blocked".to_owned()),
                    ),
                    Err(error) => error,
                };
                if let Some(key) = self.active_item_key.take() {
                    self.item_errors
                        .insert(key, format!("{}: {}", error.identifier, error.message));
                }
                self.pending = Some(Request::Search(self.search_root.clone()));
            }
            OperationResult::Mutation(result) => {
                self.finish_mutation(result.map(|result| result.identifier));
            }
            OperationResult::Ignore(result) => self.finish_mutation(result),
        }
    }

    fn finish_mutation(&mut self, result: Result<String, OperationError>) {
        self.mutation_active = false;
        match result {
            Ok(identifier) => {
                if let Some(target) = self.active_add_target.take() {
                    self.session_registered_targets.insert(target);
                }
                self.notice = Some(identifier);
            }
            Err(error) => {
                self.active_add_target = None;
                let key = match self.view {
                    View::Discover => self.active_item_key.as_ref(),
                    View::Registrations => error
                        .registration_name
                        .as_ref()
                        .or(self.active_item_key.as_ref()),
                };
                if let Some(key) = key {
                    self.item_errors.insert(
                        key.clone(),
                        format!("{}: {}", error.identifier, error.message),
                    );
                }
            }
        }
        self.active_item_key = None;
        self.pending = Some(self.reload_request());
    }

    pub fn handle(&mut self, event: Event) {
        if let Event::Resize(width, height) = event {
            self.size = (width, height);
            self.mouse_regions = MouseRegions::default();
            return;
        }
        if self.dialog.is_some() {
            self.handle_dialog(event);
            return;
        }
        if let Event::Mouse(mouse) = event {
            self.handle_mouse(mouse);
            return;
        }
        if self.filter_active {
            self.handle_filter(event);
            return;
        }
        match event {
            Event::Up => self.move_focus(-1),
            Event::Down => self.move_focus(1),
            Event::SwitchView => {
                self.view = match self.view {
                    View::Discover => View::Registrations,
                    View::Registrations => View::Discover,
                };
                self.pending = Some(self.reload_request());
            }
            Event::Toggle if !self.mutation_active => self.toggle(),
            Event::Delete if !self.mutation_active => self.delete(),
            Event::Rename
                if self.view == View::Discover && self.focused_registration().is_none() =>
            {
                if let Some(value) = self
                    .discover
                    .focused(&self.filter)
                    .map(|entry| entry.name.clone())
                {
                    self.dialog = Some(Dialog::EditName {
                        original: value.clone(),
                        value,
                    });
                }
            }
            Event::Rename if !self.mutation_active => {
                if let Some(state) = self.focused_registration() {
                    self.dialog = Some(Dialog::Rename {
                        name: state.registration.name.clone(),
                        value: state.registration.name.clone(),
                    });
                }
            }
            Event::StartFilter => self.filter_active = true,
            Event::Dismiss if !self.filter.is_empty() => self.clear_filter(),
            Event::Dismiss => {
                self.item_errors.clear();
                self.notice = None;
                self.discovery_warning = None;
            }
            Event::ClearFilter => self.clear_filter(),
            Event::Exit => self.exit = true,
            _ => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let MouseEvent {
            kind, column, row, ..
        } = mouse;
        if kind == MouseEventKind::Down(MouseButton::Left) {
            for (index, view) in [View::Discover, View::Registrations]
                .into_iter()
                .enumerate()
            {
                if contains(self.mouse_regions.tabs[index], column, row) {
                    self.filter_active = false;
                    if self.view != view {
                        self.view = view;
                        self.pending = Some(self.reload_request());
                    }
                    return;
                }
            }
            if contains(self.mouse_regions.search, column, row) {
                self.filter_active = true;
                return;
            }
        }
        let area = self.mouse_regions.list;
        if !contains(area, column, row) {
            return;
        }
        let len = self.active_list().visible_len(&self.filter);
        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.filter_active = false;
                let index = self.active_list().state.offset() + usize::from(row - area.y);
                if index < len {
                    self.active_list_mut().focus = index;
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if len > 0 => {
                let height = usize::from(area.height);
                let list = self.active_list_mut();
                let offset = if kind == MouseEventKind::ScrollUp {
                    list.state.offset().saturating_sub(3)
                } else {
                    list.state.offset().saturating_add(3)
                }
                .min(len.saturating_sub(height));
                *list.state.offset_mut() = offset;
                // Keep the selection visible so Ratatui doesn't undo wheel scrolling.
                list.focus = list.focus.clamp(offset, (offset + height - 1).min(len - 1));
            }
            _ => {}
        }
    }

    fn handle_filter(&mut self, event: Event) {
        match event {
            Event::Submit => self.filter_active = false,
            Event::ClearFilter | Event::Cancel | Event::Dismiss => {
                self.clear_filter();
                self.filter_active = false;
            }
            Event::Text(text) => {
                self.filter.push_str(&text);
                self.active_list_mut().focus = 0;
            }
            Event::Backspace => {
                self.filter.pop();
                self.active_list_mut().focus = 0;
            }
            Event::Up => self.move_focus(-1),
            Event::Down => self.move_focus(1),
            _ => {}
        }
    }

    fn clear_filter(&mut self) {
        self.filter.clear();
        self.active_list_mut().focus = 0;
    }

    fn handle_dialog(&mut self, event: Event) {
        match (&mut self.dialog, event) {
            (_, Event::Cancel | Event::Dismiss) => self.dialog = None,
            (
                Some(Dialog::EditName { value, .. }) | Some(Dialog::Rename { value, .. }),
                Event::Text(text),
            ) => value.push_str(&text),
            (
                Some(Dialog::EditName { value, .. }) | Some(Dialog::Rename { value, .. }),
                Event::Backspace,
            ) => {
                value.pop();
            }
            (Some(Dialog::EditName { value, .. }), Event::Submit) => {
                let focused_target = self
                    .discover
                    .focused(&self.filter)
                    .map(|entry| entry.target.clone());
                if let Some(entry) = self
                    .discover
                    .entries
                    .iter_mut()
                    .find(|entry| Some(&entry.target) == focused_target.as_ref())
                {
                    entry.name = value.clone();
                }
                self.discover.clamp_focus(&self.filter);
                self.dialog = None;
            }
            (Some(Dialog::Rename { name, value }), Event::Submit) if !self.mutation_active => {
                let request = Request::Rename {
                    name: name.clone(),
                    new_name: value.clone(),
                };
                self.begin_mutation(request);
                self.dialog = None;
            }
            (_, Event::Exit) => self.exit = true,
            _ => {}
        }
    }

    fn reload_request(&self) -> Request {
        match self.view {
            View::Discover => Request::Search(self.search_root.clone()),
            View::Registrations => Request::List,
        }
    }

    fn active_list(&self) -> &ItemList {
        match self.view {
            View::Discover => &self.discover,
            View::Registrations => &self.registrations,
        }
    }

    fn active_list_mut(&mut self) -> &mut ItemList {
        match self.view {
            View::Discover => &mut self.discover,
            View::Registrations => &mut self.registrations,
        }
    }

    fn focused_registration(&self) -> Option<&RegistrationState> {
        self.active_list()
            .focused(&self.filter)?
            .registration
            .as_ref()
    }

    fn move_focus(&mut self, delta: isize) {
        let filter = self.filter.clone();
        self.active_list_mut().move_focus(delta, &filter);
    }

    fn toggle(&mut self) {
        let entry = self.active_list().focused(&self.filter).cloned();
        let Some(entry) = entry else {
            return;
        };
        let request = match self.view {
            View::Discover => {
                if let Some(state) = entry.registration.as_ref() {
                    Request::Remove(state.registration.name.clone())
                } else {
                    Request::ValidateAdd {
                        target: entry.target.clone(),
                        name: entry.name.clone(),
                    }
                }
            }
            View::Registrations => {
                let Some(state) = entry.registration.as_ref() else {
                    return;
                };
                if state.registration.enabled {
                    Request::Disable(state.registration.name.clone())
                } else {
                    Request::Enable(state.registration.name.clone())
                }
            }
        };
        if let Request::ValidateAdd { target, name } = request {
            self.mutation_active = true;
            self.active_item_key = Some(target.display().to_string());
            self.pending_add = Some((target.clone(), name.clone()));
            self.pending = Some(Request::ValidateAdd { target, name });
        } else {
            self.begin_mutation(request);
            if self.view == View::Discover {
                self.active_item_key = Some(entry.target.display().to_string());
            }
        }
    }

    fn delete(&mut self) {
        let Some(entry) = self.active_list().focused(&self.filter).cloned() else {
            return;
        };
        let error_key = match self.view {
            View::Discover => entry.target.display().to_string(),
            View::Registrations => entry.name.clone(),
        };
        match entry.registration {
            Some(state) if state.registration.enabled => {
                self.begin_mutation(Request::Disable(state.registration.name));
                self.active_item_key = Some(error_key);
            }
            Some(state) => {
                self.begin_mutation(Request::Remove(state.registration.name));
                self.active_item_key = Some(error_key);
            }
            None if self.view == View::Discover => {
                self.begin_mutation(Request::Ignore(entry.target));
                self.active_item_key = Some(error_key);
            }
            None => {}
        }
    }

    fn begin_mutation(&mut self, request: Request) {
        debug_assert!(request.is_mutation());
        self.mutation_active = true;
        self.active_item_key = match &request {
            Request::Add { target, .. } | Request::ValidateAdd { target, .. } => {
                Some(target.display().to_string())
            }
            Request::Remove(name) | Request::Enable(name) | Request::Disable(name) => {
                Some(name.clone())
            }
            Request::Rename { name, .. } => Some(name.clone()),
            Request::Ignore(target) => Some(target.display().to_string()),
            Request::Search(_) | Request::List => None,
        };
        self.emitted_mutation = true;
        self.pending = Some(request);
    }

    fn display_path(&self, path: &std::path::Path) -> String {
        crate::model::display_path_with_roots(path, &self.home, &self.roots)
    }

    fn format_search_warnings(&self, warnings: &[crate::model::SearchWarning]) -> Option<String> {
        format_search_warnings(warnings, &self.home, &self.roots)
    }

    fn registration_status(state: &RegistrationState) -> String {
        let mut parts = Vec::new();
        if let Some(defect) = &state.defect {
            parts.push(defect.kind.identifier());
        }
        if !matches!(
            state.actual,
            ManagedPathKind::OwnedLink | ManagedPathKind::Missing
        ) {
            parts.push(state.actual.identifier());
        }
        parts.join(" ")
    }

    pub fn semantic_state(&self, width: u16, height: u16) -> SemanticState {
        let items: Vec<SemanticItem> = self
            .active_list()
            .entries
            .iter()
            .filter(|entry| entry_matches_filter(entry, &self.filter))
            .map(|entry| {
                let status = if entry.unavailable {
                    "unavailable".to_owned()
                } else {
                    entry
                        .registration
                        .as_ref()
                        .map(Self::registration_status)
                        .unwrap_or_default()
                };
                let error_key = match self.view {
                    View::Discover => entry.target.display().to_string(),
                    View::Registrations => entry.name.clone(),
                };
                SemanticItem {
                    name: entry.name.clone(),
                    target: entry.target.clone(),
                    checked: match self.view {
                        View::Discover => entry.registration.is_some(),
                        View::Registrations => entry
                            .registration
                            .as_ref()
                            .is_some_and(|state| state.registration.enabled),
                    },
                    status,
                    error: self.item_errors.get(&error_key).cloned(),
                }
            })
            .collect();
        let empty_message = if items.is_empty() {
            Some(
                if !self.filter.is_empty() && !self.active_list().entries.is_empty() {
                    format!("No matches for {:?}", self.filter)
                } else {
                    match self.view {
                        View::Discover => "No executable candidates found".to_owned(),
                        View::Registrations => "No Registrations".to_owned(),
                    }
                },
            )
        } else {
            None
        };
        SemanticState {
            view: self.view,
            search_root: self.search_root.clone(),
            items,
            focused: self.active_list().focus,
            hints: FOOTER_HINTS.to_owned(),
            filter: self.filter.clone(),
            filter_active: self.filter_active,
            empty_message,
            compact: width < 50 || height < 10,
            dialog: self
                .dialog
                .as_ref()
                .map(|dialog| dialog_text(dialog, &self.home, &self.roots)),
            notice: combined_notice([
                self.notice.as_deref(),
                self.path_warning.as_deref(),
                self.discovery_warning.as_deref(),
            ]),
        }
    }
}

fn format_search_warnings(
    warnings: &[crate::model::SearchWarning],
    home: &std::path::Path,
    roots: &BTreeMap<String, PathBuf>,
) -> Option<String> {
    if warnings.is_empty() {
        None
    } else {
        Some(
            warnings
                .iter()
                .map(|warning| {
                    format!(
                        "warning[{}]: {}: {}",
                        warning.kind.identifier(),
                        crate::model::display_path_with_roots(&warning.path, home, roots),
                        warning.message
                    )
                })
                .collect::<Vec<_>>()
                .join("; "),
        )
    }
}

fn combined_notice<'a>(notices: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    let mut combined = Vec::new();
    for notice in notices.into_iter().flatten() {
        if !combined.contains(&notice) {
            combined.push(notice);
        }
    }
    (!combined.is_empty()).then(|| combined.join("; "))
}

fn clamp_focus(focus: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        focus.min(len - 1)
    }
}

fn dialog_text(
    dialog: &Dialog,
    _home: &std::path::Path,
    _roots: &BTreeMap<String, PathBuf>,
) -> String {
    match dialog {
        Dialog::EditName { original, value } => format!("Edit Command Name ({original})\n{value}"),
        Dialog::Rename { value, .. } => format!("Command Name: {value}"),
        Dialog::Error { text } => text.clone(),
    }
}

pub fn initial_search_root(search_root: Option<PathBuf>, environment: &Environment) -> PathBuf {
    search_root.unwrap_or_else(|| environment.cwd().to_path_buf())
}

pub fn run(search_root: Option<PathBuf>, environment: &Environment) -> Result<(), String> {
    let root = initial_search_root(search_root, environment);
    let roots = crate::configuration::load(environment)
        .map_err(|error| error.to_string())?
        .roots;
    let mut session = TerminalSession::enter().map_err(|e| e.to_string())?;
    let mut controller = Controller::with_roots(root, environment.home().to_path_buf(), roots);
    if let Ok(diagnostic) = application::path_diagnostic(environment) {
        if diagnostic.status == PathStatus::Missing {
            controller.path_warning = Some(format!(
                "warning[{}]: {}",
                diagnostic.identifier, diagnostic.guidance
            ));
        }
    }
    loop {
        while let Some(request) = controller.take_request() {
            let result = execute_request(request, environment);
            controller.complete(result);
        }
        session
            .terminal
            .draw(|frame| render(frame, &mut controller))
            .map_err(|e| e.to_string())?;
        if controller.should_exit() {
            return Ok(());
        }
        let event = event::read().map_err(|e| e.to_string())?;
        if let Some(event) =
            translate_event(event, controller.dialog.is_some(), controller.filter_active)
        {
            controller.handle(event);
        }
    }
}

fn execute_request(request: Request, environment: &Environment) -> OperationResult {
    match request {
        Request::Search(search_root) => OperationResult::Search(
            application::search(
                SearchRequest {
                    search_root: Some(search_root),
                },
                environment,
            )
            .map_err(Into::into),
        ),
        Request::List => OperationResult::List(application::list(environment).map_err(Into::into)),
        Request::ValidateAdd { target, name } => OperationResult::Validation(
            application::validate_add(
                AddRequest {
                    target,
                    name: Some(name),
                    disabled: false,
                },
                environment,
            )
            .map_err(Into::into),
        ),
        Request::Add { target, name } => mutation_result(application::add(
            AddRequest {
                target,
                name: Some(name),
                disabled: false,
            },
            environment,
        )),
        Request::Remove(name) => mutation_result(application::remove(&name, environment)),
        Request::Enable(name) => mutation_result(application::enable(&name, environment)),
        Request::Disable(name) => mutation_result(application::disable(&name, environment)),
        Request::Rename { name, new_name } => {
            mutation_result(application::rename(&name, &new_name, environment))
        }
        Request::Ignore(target) => OperationResult::Ignore(
            application::ignore_target(&target, environment).map_err(Into::into),
        ),
    }
}

fn mutation_result(result: Result<LifecycleResult, ApplicationError>) -> OperationResult {
    OperationResult::Mutation(match result {
        Ok(result) if result.status == crate::model::LifecycleStatus::Blocked => {
            let detail = result
                .conflict
                .or_else(|| {
                    result.registration.as_ref().and_then(|state| {
                        state.defect.as_ref().map(|defect| {
                            format!(
                                "Registration {} actual={} defect={}; next safe action: {}",
                                state.registration.name,
                                state.actual.identifier(),
                                defect.kind.identifier(),
                                defect.message
                            )
                        })
                    })
                })
                .unwrap_or_else(|| {
                    "Registration operation is blocked; inspect it and retry".to_owned()
                });
            let error = OperationError::new(result.identifier, detail);
            Err(match result.registration {
                Some(state) => error.for_registration(state.registration.name),
                None => error,
            })
        }
        Ok(result) => Ok(result),
        Err(error) => Err(error.into()),
    })
}

fn translate_event(event: event::Event, dialog_open: bool, filter_active: bool) -> Option<Event> {
    match event {
        event::Event::Resize(w, h) => Some(Event::Resize(w, h)),
        event::Event::Mouse(mouse) => Some(Event::Mouse(mouse)),
        event::Event::Key(key)
            if key.kind == KeyEventKind::Press && (dialog_open || filter_active) =>
        {
            match key.code {
                KeyCode::Enter => Some(Event::Submit),
                KeyCode::Esc if filter_active => Some(Event::ClearFilter),
                KeyCode::Esc => Some(Event::Cancel),
                KeyCode::Backspace => Some(Event::Backspace),
                KeyCode::Up => Some(Event::Up),
                KeyCode::Down => Some(Event::Down),
                KeyCode::Char(c) => Some(Event::Text(c.to_string())),
                _ => None,
            }
        }
        event::Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
            KeyCode::Char('q') => Some(Event::Exit),
            KeyCode::Up | KeyCode::Char('k') => Some(Event::Up),
            KeyCode::Down | KeyCode::Char('j') => Some(Event::Down),
            KeyCode::Tab => Some(Event::SwitchView),
            KeyCode::Char('/') => Some(Event::StartFilter),
            KeyCode::Char(' ') => Some(Event::Toggle),
            KeyCode::Char('r') => Some(Event::Rename),
            KeyCode::Backspace | KeyCode::Delete => Some(Event::Delete),
            KeyCode::Esc => Some(Event::Dismiss),
            _ => None,
        },
        _ => None,
    }
}

fn render(frame: &mut Frame, controller: &mut Controller) {
    let state = controller.semantic_state(frame.size().width, frame.size().height);
    frame.render_widget(Block::default().style(theme::app()), frame.size());
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(if state.compact { 2 } else { 3 }),
        ])
        .split(frame.size());
    let header_areas = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(32), Constraint::Min(1)])
        .split(areas[0]);
    let selected = if state.view == View::Discover { 0 } else { 1 };
    let header = Block::default()
        .borders(Borders::ALL)
        .style(theme::header())
        .border_style(theme::border());
    let tab_titles = match state.view {
        View::Discover => ["[ Discover ]", "  Registry  "],
        View::Registrations => ["  Discover  ", "[ Registry ]"],
    };
    let tabs_inner = header.inner(header_areas[0]);
    let mut tab_x = tabs_inner.x;
    controller.mouse_regions = MouseRegions {
        tabs: tab_titles.map(|title| {
            // Tabs adds one cell of padding on either side and a one-cell divider.
            let width = Line::from(title).width() as u16 + 2;
            let area = Rect::new(tab_x, tabs_inner.y, width, tabs_inner.height.min(1))
                .intersection(tabs_inner);
            tab_x = tab_x.saturating_add(width + 1);
            area
        }),
        search: header_areas[1],
        list: Block::default().borders(Borders::ALL).inner(areas[1]),
    };
    frame.render_widget(
        Tabs::new(tab_titles)
            .select(selected)
            .style(theme::tab())
            .highlight_style(theme::selected_tab())
            .divider(" ")
            .block(header),
        header_areas[0],
    );
    let (search_text, search_style) = if state.filter.is_empty() && !state.filter_active {
        (" Press / to search".to_owned(), theme::search_prompt())
    } else {
        (format!(" {}", state.filter), theme::search_text())
    };
    let search_block = Block::default()
        .borders(Borders::ALL)
        .style(theme::header())
        .border_style(if state.filter_active {
            theme::dialog_border()
        } else {
            theme::border()
        })
        .title(" Search ");
    frame.render_widget(
        Paragraph::new(search_text)
            .style(search_style)
            .block(search_block),
        header_areas[1],
    );
    if state.filter_active {
        let input_width = Line::from(state.filter.as_str()).width() as u16;
        frame.set_cursor(
            header_areas[1]
                .x
                .saturating_add(2)
                .saturating_add(input_width)
                .min(header_areas[1].right().saturating_sub(2)),
            header_areas[1].y.saturating_add(1),
        );
    }
    let panel = || {
        Block::default()
            .borders(Borders::ALL)
            .style(theme::panel())
            .border_style(theme::border())
    };
    if let Some(empty) = state.empty_message {
        frame.render_widget(
            Paragraph::new(empty).style(theme::empty()).block(panel()),
            areas[1],
        );
    } else {
        let rows = state
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let marker = if item.checked { "[x]" } else { "[ ]" };
                let marker_style = if item.checked {
                    theme::checked()
                } else {
                    theme::unchecked()
                };
                let mut line = vec![
                    Span::styled(marker, marker_style),
                    Span::raw(format!(" {}", controller.display_path(&item.target))),
                ];
                if link_name_differs_from_target(item) {
                    line.push(Span::raw(" · "));
                    line.push(Span::styled(item.name.clone(), theme::alias()));
                }
                if !item.status.is_empty() {
                    line.push(Span::raw(" · "));
                    line.push(Span::styled(item.status.clone(), theme::status()));
                }
                if let Some(error) = &item.error {
                    line.push(Span::styled(format!(" · {error}"), theme::error()));
                }
                let style = if index == state.focused {
                    theme::selected_row()
                } else if controller
                    .active_list()
                    .entries
                    .iter()
                    .filter(|entry| entry_matches_filter(entry, &controller.filter))
                    .nth(index)
                    .and_then(|entry| entry.registration.as_ref())
                    .is_some_and(|registration| registration.defect.is_some())
                {
                    theme::warning()
                } else {
                    theme::panel()
                };
                ListItem::new(Line::from(line)).style(style)
            })
            .collect::<Vec<_>>();
        let list_state = &mut controller.active_list_mut().state;
        list_state.select(Some(state.focused));
        frame.render_stateful_widget(List::new(rows).block(panel()), areas[1], list_state);
    }
    frame.render_widget(
        Paragraph::new(state.hints)
            .style(theme::footer())
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .style(theme::footer())
                    .border_style(theme::border()),
            ),
        areas[2],
    );
    if let Some(text) = state.dialog {
        let area = centered_dialog_rect(70, &text, frame.size());
        let dialog = controller.dialog.as_ref().expect("dialog state exists");
        let title = dialog_title(dialog);
        let cursor_position = rename_cursor_position(dialog, area);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(text)
                .style(theme::dialog())
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .style(theme::dialog())
                        .border_style(theme::dialog_border())
                        .title_style(theme::dialog_title())
                        .title(title)
                        .padding(Padding::uniform(1)),
                ),
            area,
        );
        if let Some((x, y)) = cursor_position {
            frame.set_cursor(x, y);
        }
    }
}

fn link_name_differs_from_target(item: &SemanticItem) -> bool {
    item.target
        .file_name()
        .is_some_and(|target_name| target_name != std::ffi::OsStr::new(&item.name))
}

fn dialog_title(dialog: &Dialog) -> &'static str {
    match dialog {
        Dialog::Rename { .. } => " Rename",
        _ => "Dialog",
    }
}

fn rename_cursor_position(dialog: &Dialog, area: Rect) -> Option<(u16, u16)> {
    let Dialog::Rename { value, .. } = dialog else {
        return None;
    };
    let input_width = Line::from(format!("Command Name: {value}")).width() as u16;
    let content_start = area.x.saturating_add(2);
    let content_end = area.right().saturating_sub(3);
    Some((
        content_start.saturating_add(input_width).min(content_end),
        area.y.saturating_add(2),
    ))
}

fn centered_dialog_rect(percent_x: u16, text: &str, area: Rect) -> Rect {
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(area)[1];
    let content_width = horizontal.width.saturating_sub(4).max(1) as usize;
    let content_height = text
        .lines()
        .map(|line| Line::from(line).width().max(1).div_ceil(content_width))
        .sum::<usize>() as u16;
    let height = content_height.saturating_add(4).min(area.height);

    Rect {
        y: area
            .y
            .saturating_add(area.height.saturating_sub(height) / 2),
        height,
        ..horizontal
    }
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
            let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(error);
        }
        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(mut terminal) => match terminal.hide_cursor() {
                Ok(()) => Ok(Self { terminal }),
                Err(error) => {
                    let _ = execute!(
                        terminal.backend_mut(),
                        DisableMouseCapture,
                        LeaveAlternateScreen
                    );
                    let _ = disable_raw_mode();
                    Err(error)
                }
            },
            Err(error) => {
                let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
                let _ = disable_raw_mode();
                Err(error)
            }
        }
    }
}

trait TerminalCleanup {
    fn disable_mouse_capture(&mut self) -> io::Result<()>;
    fn show_cursor(&mut self) -> io::Result<()>;
    fn leave_alternate_screen(&mut self) -> io::Result<()>;
    fn disable_raw_mode(&mut self) -> io::Result<()>;
}

impl TerminalCleanup for TerminalSession {
    fn disable_mouse_capture(&mut self) -> io::Result<()> {
        execute!(self.terminal.backend_mut(), DisableMouseCapture)
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.terminal.show_cursor()
    }

    fn leave_alternate_screen(&mut self) -> io::Result<()> {
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen)
    }

    fn disable_raw_mode(&mut self) -> io::Result<()> {
        disable_raw_mode()
    }
}

fn restore_terminal(terminal: &mut impl TerminalCleanup) {
    let _ = terminal.disable_mouse_capture();
    let _ = terminal.show_cursor();
    let _ = terminal.leave_alternate_screen();
    let _ = terminal.disable_raw_mode();
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        restore_terminal(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingCleanup {
        calls: Vec<&'static str>,
    }

    impl TerminalCleanup for RecordingCleanup {
        fn disable_mouse_capture(&mut self) -> io::Result<()> {
            self.calls.push("disable-mouse-capture");
            Err(io::Error::other("mouse failure"))
        }

        fn show_cursor(&mut self) -> io::Result<()> {
            self.calls.push("show-cursor");
            Err(io::Error::other("cursor failure"))
        }

        fn leave_alternate_screen(&mut self) -> io::Result<()> {
            self.calls.push("leave-alternate-screen");
            Ok(())
        }

        fn disable_raw_mode(&mut self) -> io::Result<()> {
            self.calls.push("disable-raw-mode");
            Ok(())
        }
    }

    fn mouse(controller: &mut Controller, kind: MouseEventKind, column: u16, row: u16) {
        let event = event::Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: event::KeyModifiers::NONE,
        });
        controller.handle(
            translate_event(event, controller.dialog.is_some(), controller.filter_active).unwrap(),
        );
    }

    fn mouse_fixture() -> (Controller, Terminal<ratatui::backend::TestBackend>) {
        let mut controller = Controller::new(PathBuf::from("/project"));
        controller.pending = None;
        controller.discover.entries = (0..40)
            .map(|index| ListEntry {
                name: format!("tool{index:02}"),
                target: PathBuf::from(format!("/project/tool{index:02}")),
                registration: None,
                unavailable: false,
            })
            .collect();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| render(frame, &mut controller))
            .unwrap();
        (controller, terminal)
    }

    #[test]
    fn mouse_selects_tabs_and_search_without_clearing_filter() {
        let (mut controller, mut terminal) = mouse_fixture();
        mouse(
            &mut controller,
            MouseEventKind::Down(MouseButton::Left),
            40,
            1,
        );
        assert!(controller.filter_active);
        controller.handle(Event::Text("tool".into()));
        mouse(
            &mut controller,
            MouseEventKind::Down(MouseButton::Left),
            20,
            1,
        );
        assert_eq!(controller.view, View::Registrations);
        assert!(!controller.filter_active);
        assert_eq!(controller.filter, "tool");
        assert_eq!(controller.take_request(), Some(Request::List));
        terminal
            .draw(|frame| render(frame, &mut controller))
            .unwrap();
        mouse(
            &mut controller,
            MouseEventKind::Down(MouseButton::Left),
            20,
            1,
        );
        assert_eq!(controller.take_request(), None);
        mouse(
            &mut controller,
            MouseEventKind::Down(MouseButton::Left),
            5,
            1,
        );
        assert_eq!(controller.view, View::Discover);
    }

    #[test]
    fn mouse_scrolls_and_selects_using_rendered_offset() {
        let (mut controller, mut terminal) = mouse_fixture();
        mouse(&mut controller, MouseEventKind::ScrollDown, 10, 5);
        terminal
            .draw(|frame| render(frame, &mut controller))
            .unwrap();
        assert_eq!(controller.discover.state.offset(), 3);
        mouse(
            &mut controller,
            MouseEventKind::Down(MouseButton::Left),
            1,
            5,
        );
        assert_eq!(controller.discover.focus, 4);
        assert!(!controller.has_emitted_mutation());
        assert_eq!(controller.take_request(), None);
        for _ in 0..30 {
            mouse(&mut controller, MouseEventKind::ScrollDown, 10, 5);
            terminal
                .draw(|frame| render(frame, &mut controller))
                .unwrap();
        }
        assert_eq!(controller.discover.state.offset(), 24);
        for _ in 0..30 {
            mouse(&mut controller, MouseEventKind::ScrollUp, 10, 5);
            terminal
                .draw(|frame| render(frame, &mut controller))
                .unwrap();
        }
        assert_eq!(controller.discover.state.offset(), 0);
        assert_eq!(controller.discover.focus, 15);
    }

    #[test]
    fn mouse_respects_filtered_rows_blank_space_and_dialogs() {
        let (mut controller, mut terminal) = mouse_fixture();
        controller.handle(Event::StartFilter);
        controller.handle(Event::Text("tool03".into()));
        terminal
            .draw(|frame| render(frame, &mut controller))
            .unwrap();
        mouse(
            &mut controller,
            MouseEventKind::Down(MouseButton::Left),
            10,
            4,
        );
        assert!(!controller.filter_active);
        assert_eq!(
            controller
                .active_list()
                .focused(&controller.filter)
                .unwrap()
                .name,
            "tool03"
        );
        mouse(
            &mut controller,
            MouseEventKind::Down(MouseButton::Left),
            10,
            8,
        );
        assert_eq!(controller.discover.focus, 0);
        mouse(&mut controller, MouseEventKind::ScrollDown, 10, 5);
        assert_eq!(controller.discover.state.offset(), 0);
        controller.dialog = Some(Dialog::Error {
            text: "blocked".into(),
        });
        mouse(
            &mut controller,
            MouseEventKind::Down(MouseButton::Left),
            20,
            1,
        );
        mouse(
            &mut controller,
            MouseEventKind::Down(MouseButton::Left),
            40,
            1,
        );
        assert_eq!(controller.view, View::Discover);
        assert!(!controller.filter_active);
    }

    #[test]
    fn mouse_ignores_borders_other_buttons_and_stale_resize_regions() {
        let (mut controller, mut terminal) = mouse_fixture();
        for (kind, x, y) in [
            (MouseEventKind::Down(MouseButton::Left), 0, 6),
            (MouseEventKind::Down(MouseButton::Right), 10, 6),
            (MouseEventKind::Drag(MouseButton::Left), 10, 6),
            (MouseEventKind::ScrollDown, 40, 1),
        ] {
            mouse(&mut controller, kind, x, y);
        }
        assert_eq!(controller.discover.focus, 0);
        assert_eq!(controller.discover.state.offset(), 0);
        controller.handle(Event::Resize(20, 8));
        mouse(
            &mut controller,
            MouseEventKind::Down(MouseButton::Left),
            20,
            1,
        );
        assert_eq!(controller.view, View::Discover);
        terminal.backend_mut().resize(20, 8);
        terminal
            .draw(|frame| render(frame, &mut controller))
            .unwrap();
        mouse(&mut controller, MouseEventKind::ScrollDown, 10, 4);
        terminal.backend_mut().resize(1, 1);
        terminal
            .draw(|frame| render(frame, &mut controller))
            .unwrap();
        mouse(&mut controller, MouseEventKind::ScrollDown, 0, 0);
    }

    #[test]
    fn delete_keys_map_to_contextual_delete() {
        for code in [KeyCode::Backspace, KeyCode::Delete] {
            let key = event::Event::Key(crossterm::event::KeyEvent::new(
                code,
                crossterm::event::KeyModifiers::NONE,
            ));

            assert_eq!(translate_event(key, false, false), Some(Event::Delete));
        }
    }

    #[test]
    fn enter_key_has_no_list_action() {
        let key = event::Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));

        assert_eq!(translate_event(key, false, false), None);
    }

    #[test]
    fn slash_key_focuses_the_filter() {
        let key = event::Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('/'),
            crossterm::event::KeyModifiers::NONE,
        ));

        assert_eq!(translate_event(key, false, false), Some(Event::StartFilter));
    }

    #[test]
    fn escape_clears_the_active_filter() {
        let key = event::Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ));

        assert_eq!(translate_event(key, false, true), Some(Event::ClearFilter));
    }

    #[test]
    fn registry_tab_renders_enabled_and_disabled_registrations() {
        use ratatui::backend::TestBackend;

        let mut controller = Controller::new(PathBuf::from("/project"));
        controller.pending = None;
        controller.view = View::Registrations;
        controller.registrations.entries = [("enabled", true), ("disabled", false)]
            .into_iter()
            .map(|(name, enabled)| {
                ListEntry::from_registration(RegistrationState {
                    registration: crate::model::Registration {
                        name: name.to_owned(),
                        target: PathBuf::from(format!("/project/{name}")),
                        enabled,
                    },
                    managed_link: PathBuf::from(format!("/managed/{name}")),
                    actual: crate::model::ManagedPathKind::Missing,
                    observed_link_target: None,
                    defect: enabled.then(|| crate::model::RegistrationDefect {
                        kind: crate::model::RegistrationDefectKind::LinkMissing,
                        message: "Enabled Registration has no Managed Link".to_owned(),
                    }),
                })
            })
            .collect();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render(frame, &mut controller))
            .unwrap();

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("Registry"));
        assert!(!rendered.contains("Registrations"));
        assert!(rendered.contains("[x] /project/enabled"));
        assert!(rendered.contains("[ ] /project/disabled"));
    }

    #[test]
    fn tui_uses_catppuccin_mocha_for_panels_tabs_and_focus() {
        use ratatui::backend::TestBackend;

        let mut controller = Controller::new(PathBuf::from("/project"));
        controller.pending = None;
        controller.discover.entries = vec![ListEntry::from_candidate(
            Candidate {
                proposed_name: "tool".to_owned(),
                target: PathBuf::from("/project/tool"),
                registration: None,
                conflict: None,
            },
            "tool".to_owned(),
        )];
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render(frame, &mut controller))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.get(0, 0).bg, theme::mocha::MANTLE);
        assert_eq!(buffer.get(1, 4).bg, theme::mocha::SURFACE_1);
        assert_eq!(buffer.get(1, 22).bg, theme::mocha::SURFACE_0);
        let discover_start = buffer
            .content()
            .windows("Discover".len())
            .position(|window| {
                window.iter().map(|cell| cell.symbol()).collect::<String>() == "Discover"
            })
            .expect("selected Discover tab is rendered");
        let selected_tab = &buffer.content()[discover_start..discover_start + "Discover".len()];
        assert!(selected_tab.iter().all(|cell| {
            cell.fg == theme::mocha::MAUVE
                && cell.bg == theme::mocha::SURFACE_1
                && cell.modifier.contains(ratatui::style::Modifier::UNDERLINED)
        }));
        assert_eq!(buffer.content()[discover_start - 2].symbol(), "[");
        assert_eq!(
            buffer.content()[discover_start + "Discover".len() + 1].symbol(),
            "]"
        );
        let header_text: String = (0..3)
            .flat_map(|y| (0..80).map(move |x| buffer.get(x, y).symbol()))
            .collect();
        assert!(!header_text.contains("bin"));
        assert!(!header_text.contains("/project"));
    }

    #[test]
    fn filter_is_in_the_header_and_has_leading_inner_space() {
        use ratatui::backend::{Backend, TestBackend};

        let mut controller = Controller::new(PathBuf::from("/project"));
        controller.pending = None;
        controller.filter = "abc".to_owned();
        controller.filter_active = true;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render(frame, &mut controller))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.get(33, 1).symbol(), " ");
        assert_eq!(buffer.get(34, 1).symbol(), "a");
        assert_eq!(terminal.backend_mut().get_cursor().unwrap(), (37, 1));
    }

    #[test]
    fn only_the_differing_link_name_uses_catppuccin_mocha_green() {
        use ratatui::backend::TestBackend;

        let mut controller = Controller::new(PathBuf::from("/project"));
        controller.pending = None;
        controller.view = View::Registrations;
        controller.registrations.entries = vec![ListEntry::from_registration(RegistrationState {
            registration: crate::model::Registration {
                name: "alias".to_owned(),
                target: PathBuf::from("/project/tool"),
                enabled: true,
            },
            managed_link: PathBuf::from("/managed/alias"),
            actual: crate::model::ManagedPathKind::OwnedLink,
            observed_link_target: Some(PathBuf::from("/project/tool")),
            defect: None,
        })];
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render(frame, &mut controller))
            .unwrap();

        let cells = terminal.backend().buffer().content();
        let alias_start = cells
            .windows(5)
            .position(|window| {
                window.iter().map(|cell| cell.symbol()).collect::<String>() == "alias"
            })
            .expect("differing link name is rendered");
        assert!(cells[alias_start..alias_start + 5]
            .iter()
            .all(|cell| cell.fg == theme::mocha::GREEN));
        assert!(cells[alias_start - 3..alias_start]
            .iter()
            .all(|cell| cell.fg != theme::mocha::GREEN));
    }

    #[test]
    fn matching_link_name_is_not_repeated() {
        let item = SemanticItem {
            name: "tool".to_owned(),
            target: PathBuf::from("/project/tool"),
            checked: true,
            status: String::new(),
            error: None,
        };

        assert!(!link_name_differs_from_target(&item));
    }

    #[test]
    fn healthy_registration_row_does_not_use_unhealthy_styling() {
        use ratatui::backend::TestBackend;

        let mut controller = Controller::new(PathBuf::from("/project"));
        controller.pending = None;
        controller.view = View::Registrations;
        controller.registrations.entries = ["first", "second"]
            .into_iter()
            .map(|name| {
                ListEntry::from_registration(RegistrationState {
                    registration: crate::model::Registration {
                        name: name.to_owned(),
                        target: PathBuf::from(format!("/project/{name}")),
                        enabled: true,
                    },
                    managed_link: PathBuf::from(format!("/managed/{name}")),
                    actual: crate::model::ManagedPathKind::OwnedLink,
                    observed_link_target: Some(PathBuf::from(format!("/project/{name}"))),
                    defect: None,
                })
            })
            .collect();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render(frame, &mut controller))
            .unwrap();

        assert!(terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .all(|cell| cell.fg != theme::mocha::YELLOW));
    }

    #[test]
    fn footer_contains_only_hotkeys() {
        use ratatui::backend::TestBackend;

        let mut controller = Controller::new(PathBuf::from("/project"));
        controller.pending = None;
        controller.notice = Some("registration-removed".to_owned());
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render(frame, &mut controller))
            .unwrap();

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!rendered.contains("registration-removed"));
        assert!(!rendered.contains("↑↓ move"));
        assert!(rendered.contains(" tab    space    / search    r rename    del remove    q quit"));
    }

    #[test]
    fn search_prompt_is_dimmer_than_entered_text() {
        use ratatui::backend::TestBackend;

        let mut controller = Controller::new(PathBuf::from("/project"));
        controller.pending = None;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render(frame, &mut controller))
            .unwrap();
        assert_eq!(terminal.backend().buffer().get(34, 1).symbol(), "P");
        assert_eq!(
            terminal.backend().buffer().get(34, 1).fg,
            theme::mocha::OVERLAY_0
        );

        controller.filter = "query".to_owned();
        terminal
            .draw(|frame| render(frame, &mut controller))
            .unwrap();
        assert_eq!(terminal.backend().buffer().get(34, 1).symbol(), "q");
        assert_eq!(
            terminal.backend().buffer().get(34, 1).fg,
            theme::mocha::TEXT
        );
    }

    #[test]
    fn overflowing_list_scrolls_to_keep_the_focused_item_visible() {
        use ratatui::backend::TestBackend;

        let mut controller = Controller::new(PathBuf::from("/project"));
        controller.pending = None;
        controller.discover.entries = (0..10)
            .map(|index| {
                let name = format!("tool-{index}");
                ListEntry::from_candidate(
                    Candidate {
                        proposed_name: name.clone(),
                        target: PathBuf::from(format!("/project/tool-{index}")),
                        registration: None,
                        conflict: None,
                    },
                    name,
                )
            })
            .collect();
        controller.discover.focus = 9;

        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, &mut controller))
            .unwrap();

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("tool-9"));
        assert!(!rendered.contains("tool-0"));
    }

    #[test]
    fn rename_dialog_is_content_height_centered_with_an_indented_title_and_visible_input_cursor() {
        use ratatui::backend::{Backend, TestBackend};

        let mut controller = Controller::new(PathBuf::from("/project"));
        controller.pending = None;
        controller.dialog = Some(Dialog::Rename {
            name: "old".to_owned(),
            value: "old".to_owned(),
        });
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render(frame, &mut controller))
            .unwrap();

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("Rename"));
        assert!(!rendered.contains("Rename Registration old"));
        assert!(rendered.contains("Command Name: old"));
        assert_eq!(
            centered_dialog_rect(70, "Command Name: old", Rect::new(0, 0, 80, 24)),
            Rect::new(12, 9, 56, 5)
        );
        assert_eq!(terminal.backend().buffer().get(14, 9).symbol(), "R");
        assert_eq!(terminal.backend_mut().get_cursor().unwrap(), (31, 11));
    }

    #[test]
    fn terminal_restoration_attempts_every_step_even_after_an_error() {
        let mut terminal = RecordingCleanup::default();

        restore_terminal(&mut terminal);

        assert_eq!(
            terminal.calls,
            [
                "disable-mouse-capture",
                "show-cursor",
                "leave-alternate-screen",
                "disable-raw-mode"
            ]
        );
    }
}
