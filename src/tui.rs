use crate::project::{self, Project, Repo, ResolvedFile, Source};
use crate::{Result, envfile};
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

enum Mode {
    Normal,
    Help,
    Notice(String),
    RemovalBlocked(PathBuf, Vec<PathBuf>, usize),
    Prompt(Prompt, String),
    Confirm(Confirm),
    Import {
        paths: Vec<PathBuf>,
        marked: BTreeSet<usize>,
        index: usize,
    },
}

enum Prompt {
    Init,
    Add,
    Filter,
    NewKey,
    Value(String),
}
enum Confirm {
    Remove(PathBuf),
    Unset(PathBuf, String),
    Relink(PathBuf, bool),
}
enum Outcome {
    Continue,
    Quit,
    Edit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pane {
    Files,
    Keys,
    Details,
}

#[derive(Clone, Copy)]
enum Click {
    Key(char),
    Source(Source),
    Refresh,
    Help,
    Quit,
    Confirm,
    Cancel,
}

struct App {
    cwd: PathBuf,
    repo: Option<Repo>,
    project: Option<Project>,
    available: Vec<Project>,
    project_index: usize,
    files: Vec<PathBuf>,
    file_index: usize,
    keys: Vec<String>,
    key_index: usize,
    source: Source,
    source_prefs: BTreeMap<PathBuf, Source>,
    managed: BTreeSet<PathBuf>,
    file_status: BTreeMap<PathBuf, Result<project::LinkStatus>>,
    file_labels: BTreeMap<PathBuf, &'static str>,
    values: BTreeMap<String, String>,
    resolved: Option<ResolvedFile>,
    focus_keys: bool,
    focus_details: bool,
    reveal: bool,
    value_scroll: u16,
    help_scroll: u16,
    file_error: Option<String>,
    no_color: bool,
    compact: bool,
    filter: String,
    mode: Mode,
    message: String,
}

#[derive(Default)]
struct HitAreas {
    files: Rect,
    keys: Rect,
    details: Rect,
    file_offset: usize,
    key_offset: usize,
    modal_list: Rect,
    modal_offset: usize,
    clicks: Vec<(Rect, Click)>,
}

impl HitAreas {
    fn contains(area: Rect, column: u16, row: u16) -> bool {
        column >= area.x
            && column < area.x.saturating_add(area.width)
            && row >= area.y
            && row < area.y.saturating_add(area.height)
    }

    fn item_at(area: Rect, offset: usize, column: u16, row: u16) -> Option<usize> {
        if !Self::contains(area, column, row) {
            return None;
        }
        Some(offset + usize::from(row - area.y))
    }
}

struct Theme {
    accent: Color,
    muted: Color,
    warning: Color,
    error: Color,
}

impl Theme {
    fn new(no_color: bool) -> Self {
        if no_color {
            Self {
                accent: Color::Reset,
                muted: Color::Reset,
                warning: Color::Reset,
                error: Color::Reset,
            }
        } else {
            Self {
                accent: Color::Cyan,
                muted: Color::DarkGray,
                warning: Color::Yellow,
                error: Color::Red,
            }
        }
    }
}

fn content_area(screen: Rect) -> Rect {
    let width = screen.width.saturating_sub(2).min(132);
    let height = screen.height.saturating_sub(2).min(34);
    Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    )
}

impl App {
    fn pane(&self) -> Pane {
        if self.focus_details {
            Pane::Details
        } else if self.focus_keys {
            Pane::Keys
        } else {
            Pane::Files
        }
    }

    fn focus(&mut self, pane: Pane) {
        if self.pane() != pane {
            self.reveal = false;
            self.value_scroll = 0;
        }
        self.focus_keys = pane == Pane::Keys;
        self.focus_details = pane == Pane::Details;
    }

    fn select_source(&mut self, source: Source) {
        self.source = source;
        if let Some(file) = self.selected_file()
            && let Some(repo) = &self.repo
        {
            match project::save_ui_source(repo, &file, source) {
                Ok(()) => {
                    if source == Source::Auto {
                        self.source_prefs.remove(&file);
                    } else {
                        self.source_prefs.insert(file, source);
                    }
                    self.message.clear();
                }
                Err(error) => {
                    self.message =
                        format!("source selected for this session; cannot save preference: {error}")
                }
            }
        }
        self.refresh_keys();
    }

    fn restore_source(&mut self) {
        self.source = self
            .selected_file()
            .and_then(|file| self.source_prefs.get(&file).copied())
            .unwrap_or(Source::Auto);
    }

    fn selected_status(&self) -> &'static str {
        self.selected_file()
            .and_then(|file| self.file_labels.get(&file).copied())
            .unwrap_or("")
    }

    fn source_label(&self) -> String {
        let policy = match self.source {
            Source::Auto => "AUTO",
            Source::Local => "LOCAL",
            Source::Shared => "SHARED",
        };
        self.resolved.as_ref().map_or_else(
            || policy.into(),
            |resolved| {
                format!(
                    "{policy} -> {}",
                    if resolved.shared { "SHARED" } else { "LOCAL" }
                )
            },
        )
    }

    fn new(cwd: &Path) -> Result<Self> {
        let repo = project::discover_repo(cwd)?;
        let source_prefs = repo
            .as_ref()
            .map(project::load_ui_sources)
            .transpose()?
            .unwrap_or_default();
        let current = repo.as_ref().map(|r| r.project.clone());
        let mut app = Self {
            cwd: cwd.to_path_buf(),
            repo,
            project: current,
            available: project::projects()?,
            project_index: 0,
            files: Vec::new(),
            file_index: 0,
            keys: Vec::new(),
            key_index: 0,
            source: Source::Auto,
            source_prefs,
            managed: BTreeSet::new(),
            file_status: BTreeMap::new(),
            file_labels: BTreeMap::new(),
            values: BTreeMap::new(),
            resolved: None,
            focus_keys: false,
            focus_details: false,
            reveal: false,
            value_scroll: 0,
            help_scroll: 0,
            file_error: None,
            no_color: env::var_os("NO_COLOR").is_some(),
            compact: false,
            filter: String::new(),
            mode: Mode::Normal,
            message: String::new(),
        };
        app.refresh()?;
        Ok(app)
    }

    fn visible_files(&self) -> Vec<PathBuf> {
        self.files
            .iter()
            .filter(|path| path.to_string_lossy().contains(&self.filter))
            .cloned()
            .collect()
    }

    fn selected_file(&self) -> Option<PathBuf> {
        self.visible_files().get(self.file_index).cloned()
    }
    fn selected_key(&self) -> Option<String> {
        self.keys.get(self.key_index).cloned()
    }
    fn selected_is_managed(&self) -> bool {
        self.selected_file()
            .is_some_and(|file| self.managed.contains(&file))
    }

    fn resolve_selected(&self) -> Result<ResolvedFile> {
        let file = self.selected_file().ok_or("no file selected")?;
        if let Some(repo) = &self.repo {
            project::resolve_file(repo, &file, self.source)
        } else {
            Ok(ResolvedFile {
                path: project::managed_file(self.project()?, &file)?,
                shared: true,
            })
        }
    }

    fn edit_key(&self, file: &Path, key: &str, value: Option<&str>) -> Result<()> {
        if let Some(repo) = &self.repo {
            project::edit_file(repo, file, self.source, key, value)
        } else if let Some(value) = value {
            project::set_key(self.project()?, file, key, value)
        } else {
            project::unset_key(self.project()?, file, key)
        }
    }

    fn project(&self) -> Result<&Project> {
        self.project.as_ref().ok_or("select a project first".into())
    }
    fn repo(&self) -> Result<&Repo> {
        self.repo
            .as_ref()
            .ok_or("open a registered Git repository for this action".into())
    }

    fn refresh(&mut self) -> Result<()> {
        let selected_before = self.selected_file();
        self.available = project::projects()?;
        self.managed.clear();
        self.file_status.clear();
        self.file_labels.clear();
        if let Some(selected) = &self.project {
            self.files = project::files(selected)?;
            self.managed.extend(self.files.iter().cloned());
            if let Some(repo) = &self.repo {
                for file in &self.files {
                    self.file_status
                        .insert(file.clone(), project::status(repo, file));
                }
            }
            if let Some(repo) = &self.repo
                && repo.project.name == selected.name
            {
                self.files.extend(project::local_files(repo)?);
                self.files.sort();
                self.files.dedup();
            }
            for file in &self.files {
                let label = if !self.managed.contains(file) {
                    "LOCAL"
                } else {
                    match self.file_status.get(file) {
                        Some(Ok(project::LinkStatus::Linked)) => "LINKED",
                        Some(Ok(project::LinkStatus::Missing)) => "STORED",
                        Some(Ok(project::LinkStatus::AbsentDirectory)) => "NO DIR",
                        Some(Ok(project::LinkStatus::Conflict)) => {
                            if self.repo.as_ref().is_some_and(|repo| {
                                project::resolve_file(repo, file, Source::Local).is_ok()
                            }) {
                                "BOTH"
                            } else {
                                "CONFLICT"
                            }
                        }
                        Some(Err(_)) => "ERROR",
                        None => "STORED",
                    }
                };
                self.file_labels.insert(file.clone(), label);
            }
        } else {
            self.files.clear();
        }
        let visible = self.visible_files();
        self.file_index = selected_before
            .and_then(|old| visible.iter().position(|file| *file == old))
            .unwrap_or_else(|| self.file_index.min(visible.len().saturating_sub(1)));
        self.restore_source();
        self.refresh_keys();
        Ok(())
    }

    fn refresh_keys(&mut self) {
        self.keys.clear();
        self.values.clear();
        self.resolved = None;
        self.file_error = None;
        if self.selected_file().is_some() {
            match self.resolve_selected() {
                Ok(file) => {
                    let result = envfile::read_vars(&file.path);
                    self.resolved = Some(file);
                    match result {
                        Ok(values) => {
                            self.values.extend(values);
                            self.keys.extend(self.values.keys().cloned());
                        }
                        Err(error) => self.file_error = Some(error),
                    }
                }
                Err(error) => self.file_error = Some(error),
            }
        }
        self.key_index = self.key_index.min(self.keys.len().saturating_sub(1));
        self.reveal = false;
        self.value_scroll = 0;
    }

    fn set_message(&mut self, result: Result<String>) {
        self.message = result.unwrap_or_else(|error| error);
        if let Err(error) = self.refresh() {
            self.message = error;
        }
    }

    fn handle(&mut self, key: KeyEvent) -> Outcome {
        if key.kind == KeyEventKind::Release {
            return Outcome::Continue;
        }
        let mode = std::mem::replace(&mut self.mode, Mode::Normal);
        match mode {
            Mode::Normal => self.handle_normal(key),
            Mode::Notice(message) => {
                if key.code != KeyCode::Esc && key.code != KeyCode::Enter {
                    self.mode = Mode::Notice(message);
                }
                Outcome::Continue
            }
            Mode::Help => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => self.reveal = false,
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.help_scroll = self.help_scroll.saturating_add(1);
                        self.mode = Mode::Help;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.help_scroll = self.help_scroll.saturating_sub(1);
                        self.mode = Mode::Help;
                    }
                    _ => self.mode = Mode::Help,
                }
                Outcome::Continue
            }
            Mode::RemovalBlocked(file, worktrees, mut index) => {
                match key.code {
                    KeyCode::Down | KeyCode::Char('j') => {
                        index = (index + 1).min(worktrees.len().saturating_sub(1))
                    }
                    KeyCode::Up | KeyCode::Char('k') => index = index.saturating_sub(1),
                    _ => return Outcome::Continue,
                }
                self.mode = Mode::RemovalBlocked(file, worktrees, index);
                Outcome::Continue
            }
            Mode::Prompt(kind, mut input) => {
                match key.code {
                    KeyCode::Esc => {}
                    KeyCode::Enter => self.finish_prompt(kind, input),
                    KeyCode::Backspace => {
                        input.pop();
                        self.mode = Mode::Prompt(kind, input);
                    }
                    KeyCode::Char(c) if !c.is_control() => {
                        input.push(c);
                        self.mode = Mode::Prompt(kind, input);
                    }
                    _ => self.mode = Mode::Prompt(kind, input),
                }
                Outcome::Continue
            }
            Mode::Confirm(confirm) => {
                if key.code == KeyCode::Char('y') {
                    self.finish_confirm(confirm);
                }
                Outcome::Continue
            }
            Mode::Import {
                paths,
                mut marked,
                mut index,
            } => {
                match key.code {
                    KeyCode::Esc => {}
                    KeyCode::Up | KeyCode::Char('k') => {
                        index = index.saturating_sub(1);
                        self.mode = Mode::Import {
                            paths,
                            marked,
                            index,
                        };
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        index = (index + 1).min(paths.len().saturating_sub(1));
                        self.mode = Mode::Import {
                            paths,
                            marked,
                            index,
                        };
                    }
                    KeyCode::Char(' ') => {
                        if !marked.insert(index) {
                            marked.remove(&index);
                        }
                        self.mode = Mode::Import {
                            paths,
                            marked,
                            index,
                        };
                    }
                    KeyCode::Enter => {
                        let result = (|| {
                            let repo = self.repo()?;
                            let mut count = 0;
                            for at in marked {
                                project::add(repo, &repo.root, &paths[at])?;
                                count += 1;
                            }
                            Ok(format!("imported {count} files"))
                        })();
                        self.set_message(result);
                    }
                    _ => {
                        self.mode = Mode::Import {
                            paths,
                            marked,
                            index,
                        }
                    }
                }
                Outcome::Continue
            }
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, areas: &HitAreas) -> Outcome {
        let point = (mouse.column, mouse.row);
        if mouse.kind == MouseEventKind::Down(event::MouseButton::Left) {
            if let Some((_, click)) = areas
                .clicks
                .iter()
                .rev()
                .find(|(area, _)| HitAreas::contains(*area, point.0, point.1))
            {
                let click = *click;
                return match click {
                    Click::Key(ch) => {
                        if matches!(self.mode, Mode::Normal)
                            && matches!(ch, 's' | 'x' | 'n' | 'e' | 'r')
                        {
                            self.focus(Pane::Details);
                        }
                        self.handle(KeyEvent::new(KeyCode::Char(ch), event::KeyModifiers::NONE))
                    }
                    Click::Source(source) => {
                        if matches!(self.mode, Mode::Normal) {
                            self.select_source(source);
                            self.focus(Pane::Details);
                        }
                        Outcome::Continue
                    }
                    Click::Refresh => {
                        self.handle(KeyEvent::new(KeyCode::F(5), event::KeyModifiers::NONE))
                    }
                    Click::Help => {
                        self.handle(KeyEvent::new(KeyCode::Char('?'), event::KeyModifiers::NONE))
                    }
                    Click::Quit => {
                        self.handle(KeyEvent::new(KeyCode::Char('q'), event::KeyModifiers::NONE))
                    }
                    Click::Confirm => {
                        let key = if matches!(self.mode, Mode::Confirm(_)) {
                            KeyCode::Char('y')
                        } else {
                            KeyCode::Enter
                        };
                        self.handle(KeyEvent::new(key, event::KeyModifiers::NONE))
                    }
                    Click::Cancel => {
                        self.handle(KeyEvent::new(KeyCode::Esc, event::KeyModifiers::NONE))
                    }
                };
            }
            if HitAreas::contains(areas.modal_list, point.0, point.1) {
                if let Some(index) =
                    HitAreas::item_at(areas.modal_list, areas.modal_offset, point.0, point.1)
                {
                    match &mut self.mode {
                        Mode::Import {
                            paths,
                            index: selected,
                            ..
                        } if index < paths.len() => {
                            *selected = index;
                            return self.handle(KeyEvent::new(
                                KeyCode::Char(' '),
                                event::KeyModifiers::NONE,
                            ));
                        }
                        Mode::RemovalBlocked(_, paths, selected) if index < paths.len() => {
                            *selected = index
                        }
                        _ => {}
                    }
                }
                return Outcome::Continue;
            }
            if !matches!(self.mode, Mode::Normal) {
                return Outcome::Continue;
            }
            if self.project.is_none() {
                if let Some(index) =
                    HitAreas::item_at(areas.files, areas.file_offset, point.0, point.1)
                    && index < self.available.len()
                {
                    self.project_index = index;
                }
                return Outcome::Continue;
            }
            if let Some(index) = HitAreas::item_at(areas.keys, areas.key_offset, point.0, point.1)
                && index < self.keys.len()
            {
                self.focus(Pane::Keys);
                self.key_index = index;
                self.reveal = false;
            } else if let Some(index) =
                HitAreas::item_at(areas.files, areas.file_offset, point.0, point.1)
                && index < self.visible_files().len()
            {
                self.focus(Pane::Files);
                if self.file_index != index {
                    self.file_index = index;
                    self.restore_source();
                    self.refresh_keys();
                }
            } else if HitAreas::contains(areas.details, point.0, point.1) {
                self.focus(Pane::Details);
            }
        } else if matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            let down = mouse.kind == MouseEventKind::ScrollDown;
            match &mut self.mode {
                Mode::Help => {
                    self.help_scroll = if down {
                        self.help_scroll.saturating_add(1)
                    } else {
                        self.help_scroll.saturating_sub(1)
                    }
                }
                Mode::Import { paths, index, .. } | Mode::RemovalBlocked(_, paths, index)
                    if HitAreas::contains(areas.modal_list, point.0, point.1) =>
                {
                    *index = if down {
                        (*index + 1).min(paths.len().saturating_sub(1))
                    } else {
                        index.saturating_sub(1)
                    };
                }
                Mode::Normal if self.project.is_some() => {
                    if HitAreas::contains(areas.keys, point.0, point.1) {
                        self.focus(Pane::Keys);
                        self.key_index = if down {
                            (self.key_index + 1).min(self.keys.len().saturating_sub(1))
                        } else {
                            self.key_index.saturating_sub(1)
                        };
                        self.reveal = false;
                    } else if HitAreas::contains(areas.files, point.0, point.1) {
                        self.focus(Pane::Files);
                        let index = if down {
                            (self.file_index + 1).min(self.visible_files().len().saturating_sub(1))
                        } else {
                            self.file_index.saturating_sub(1)
                        };
                        if self.file_index != index {
                            self.file_index = index;
                            self.restore_source();
                            self.refresh_keys();
                        }
                    } else if HitAreas::contains(areas.details, point.0, point.1) && self.reveal {
                        self.focus(Pane::Details);
                        self.value_scroll = if down {
                            self.value_scroll.saturating_add(1)
                        } else {
                            self.value_scroll.saturating_sub(1)
                        };
                    }
                }
                _ => {}
            }
        }
        Outcome::Continue
    }

    fn handle_normal(&mut self, key: KeyEvent) -> Outcome {
        if self.project.is_none() {
            match key.code {
                KeyCode::Char('q') => return Outcome::Quit,
                KeyCode::Char('?') => self.mode = Mode::Help,
                KeyCode::Down | KeyCode::Char('j') => {
                    self.project_index =
                        (self.project_index + 1).min(self.available.len().saturating_sub(1))
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.project_index = self.project_index.saturating_sub(1)
                }
                KeyCode::Enter => {
                    self.project = self.available.get(self.project_index).cloned();
                    self.set_message(Ok("project opened".into()));
                }
                KeyCode::Char('n') => self.mode = Mode::Prompt(Prompt::Init, String::new()),
                KeyCode::Char('a') => {
                    let result = (|| {
                        let selected = self
                            .available
                            .get(self.project_index)
                            .ok_or("no project selected")?;
                        project::attach(&self.cwd, &selected.name)
                    })();
                    match result {
                        Ok(repo) => {
                            self.project = Some(repo.project.clone());
                            self.repo = Some(repo);
                            self.set_message(Ok("repository attached".into()));
                        }
                        Err(error) => self.message = error,
                    }
                }
                _ => {}
            }
            return Outcome::Continue;
        }

        if matches!(key.code, KeyCode::Char('d' | 'u'))
            && self.selected_file().is_some()
            && !self.selected_is_managed()
        {
            self.message =
                "This is a local file; only shared files can be unlinked or removed from the store"
                    .into();
            return Outcome::Continue;
        }
        if matches!(key.code, KeyCode::Char('n' | 's' | 'x' | 'e')) && self.resolved.is_none() {
            self.message = "Choose a valid source with v before editing".into();
            return Outcome::Continue;
        }
        if matches!(key.code, KeyCode::Char('n' | 's' | 'x')) && self.file_error.is_some() {
            self.message = "Repair this file with e before editing keys".into();
            return Outcome::Continue;
        }
        match key.code {
            KeyCode::Char('?') => {
                self.reveal = false;
                self.mode = Mode::Help;
                Outcome::Continue
            }
            KeyCode::F(5) => {
                self.set_message(Ok("refreshed".into()));
                Outcome::Continue
            }
            KeyCode::Char('v') => {
                let next = match self.source {
                    Source::Auto => Source::Local,
                    Source::Local => Source::Shared,
                    Source::Shared => Source::Auto,
                };
                self.select_source(next);
                Outcome::Continue
            }
            KeyCode::Char('q') => Outcome::Quit,
            KeyCode::Esc => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.file_index = 0;
                    self.restore_source();
                    self.refresh_keys();
                } else {
                    self.focus(Pane::Files);
                }
                Outcome::Continue
            }
            KeyCode::Tab => {
                let next = match self.pane() {
                    Pane::Files => Pane::Keys,
                    Pane::Keys => Pane::Details,
                    Pane::Details => Pane::Files,
                };
                self.focus(next);
                Outcome::Continue
            }
            KeyCode::BackTab => {
                let next = match self.pane() {
                    Pane::Files => Pane::Details,
                    Pane::Keys => Pane::Files,
                    Pane::Details => Pane::Keys,
                };
                self.focus(next);
                Outcome::Continue
            }
            KeyCode::Right => {
                self.focus(match self.pane() {
                    Pane::Files => Pane::Keys,
                    Pane::Keys => Pane::Details,
                    Pane::Details => Pane::Details,
                });
                Outcome::Continue
            }
            KeyCode::Left => {
                self.focus(match self.pane() {
                    Pane::Files => Pane::Files,
                    Pane::Keys => Pane::Files,
                    Pane::Details => Pane::Keys,
                });
                Outcome::Continue
            }
            KeyCode::Enter if self.compact => {
                self.focus(match self.pane() {
                    Pane::Files => Pane::Keys,
                    Pane::Keys => Pane::Details,
                    Pane::Details => Pane::Details,
                });
                Outcome::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.focus_details {
                    if self.reveal {
                        self.value_scroll = self.value_scroll.saturating_add(1);
                    }
                } else if self.focus_keys {
                    self.key_index = (self.key_index + 1).min(self.keys.len().saturating_sub(1));
                    self.reveal = false;
                } else {
                    self.file_index =
                        (self.file_index + 1).min(self.visible_files().len().saturating_sub(1));
                    self.restore_source();
                    self.refresh_keys();
                }
                Outcome::Continue
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.focus_details {
                    if self.reveal {
                        self.value_scroll = self.value_scroll.saturating_sub(1);
                    }
                } else if self.focus_keys {
                    self.key_index = self.key_index.saturating_sub(1);
                    self.reveal = false;
                } else {
                    self.file_index = self.file_index.saturating_sub(1);
                    self.restore_source();
                    self.refresh_keys();
                }
                Outcome::Continue
            }
            KeyCode::Char('/') => {
                self.mode = Mode::Prompt(Prompt::Filter, self.filter.clone());
                Outcome::Continue
            }
            KeyCode::Char('r') => {
                if self.selected_key().is_some() {
                    self.reveal = !self.reveal;
                    self.value_scroll = 0;
                }
                Outcome::Continue
            }
            KeyCode::Char('n') => {
                if self.selected_file().is_some() {
                    self.mode = Mode::Prompt(Prompt::NewKey, String::new());
                }
                Outcome::Continue
            }
            KeyCode::Char('s') => {
                if let Some(key) = self.selected_key() {
                    self.mode = Mode::Prompt(Prompt::Value(key), String::new());
                }
                Outcome::Continue
            }
            KeyCode::Char('x') => {
                if let (Some(file), Some(key)) = (self.selected_file(), self.selected_key()) {
                    self.mode = Mode::Confirm(Confirm::Unset(file, key));
                }
                Outcome::Continue
            }
            KeyCode::Char('d') => {
                if let Some(file) = self.selected_file() {
                    match self
                        .repo()
                        .and_then(|repo| project::removal_blockers(repo, &file))
                    {
                        Ok(blockers) if blockers.is_empty() => {
                            self.mode = Mode::Confirm(Confirm::Remove(file))
                        }
                        Ok(blockers) => self.mode = Mode::RemovalBlocked(file, blockers, 0),
                        Err(error) => self.message = error,
                    }
                }
                Outcome::Continue
            }
            KeyCode::Char('a') => {
                self.mode = Mode::Prompt(Prompt::Add, String::new());
                Outcome::Continue
            }
            KeyCode::Char('i') => {
                match self.repo().and_then(project::scan) {
                    Ok(paths) if paths.is_empty() => {
                        self.message = "no ignored env files to import".into()
                    }
                    Ok(paths) => {
                        self.mode = Mode::Import {
                            paths,
                            marked: BTreeSet::new(),
                            index: 0,
                        }
                    }
                    Err(error) => self.message = error,
                }
                Outcome::Continue
            }
            KeyCode::Char('l') => {
                if self.selected_status() == "BOTH" {
                    if let Some(file) = self.selected_file() {
                        match self
                            .repo()
                            .and_then(|repo| project::relink_preview(repo, &file))
                        {
                            Ok(differs) => {
                                self.mode = Mode::Confirm(Confirm::Relink(file, differs))
                            }
                            Err(error) => self.message = error,
                        }
                    }
                } else {
                    let result = self.repo().and_then(|repo| {
                        project::link(repo)
                            .map(|(linked, skipped)| format!("linked {linked}, skipped {skipped}"))
                    });
                    self.set_message(result);
                }
                Outcome::Continue
            }
            KeyCode::Char('u') => {
                let result = (|| {
                    let file = self.selected_file().ok_or("no file selected")?;
                    project::unlink(self.repo()?, &file)?;
                    Ok(format!("unlinked {}", file.display()))
                })();
                self.set_message(result);
                Outcome::Continue
            }
            KeyCode::Char('e') => {
                if self.selected_file().is_some() {
                    Outcome::Edit
                } else {
                    Outcome::Continue
                }
            }
            _ => Outcome::Continue,
        }
    }

    fn finish_prompt(&mut self, prompt: Prompt, input: String) {
        match prompt {
            Prompt::Init => match project::init(&self.cwd, &input) {
                Ok(repo) => {
                    self.project = Some(repo.project.clone());
                    self.repo = Some(repo);
                    self.set_message(Ok("project registered".into()));
                }
                Err(error) => self.message = error,
            },
            Prompt::Add => {
                let result = (|| {
                    let rel = project::add(self.repo()?, &self.cwd, Path::new(&input))?;
                    Ok(format!("imported {}", rel.display()))
                })();
                self.set_message(result);
            }
            Prompt::Filter => {
                self.filter = input;
                self.file_index = 0;
                self.restore_source();
                self.refresh_keys();
            }
            Prompt::NewKey => {
                if !input.is_empty() {
                    self.mode = Mode::Prompt(Prompt::Value(input), String::new());
                }
            }
            Prompt::Value(key) => {
                let result = (|| {
                    let file = self.selected_file().ok_or("no file selected")?;
                    self.edit_key(&file, &key, Some(&input))?;
                    Ok(format!("saved key {key}"))
                })();
                self.set_message(result);
            }
        }
    }

    fn finish_confirm(&mut self, confirm: Confirm) {
        if let Confirm::Relink(file, _) = confirm {
            let result = self.repo().and_then(|repo| project::relink(repo, &file));
            match result {
                Ok(Some(backup)) => {
                    self.set_message(Ok(format!("relinked {}", file.display())));
                    self.mode = Mode::Notice(format!(
                        "Relinked {}. The previous local copy differed from the shared file and was saved privately at {}. Press Esc to close.",
                        file.display(),
                        backup.display()
                    ));
                }
                Ok(None) => {
                    self.set_message(Ok(format!("relinked {}; copies matched", file.display())))
                }
                Err(error) => self.set_message(Err(error)),
            }
            return;
        }
        let result = match confirm {
            Confirm::Remove(file) => (|| {
                project::remove(self.repo()?, &file)?;
                Ok(format!(
                    "removed shared {}; local copies are preserved",
                    file.display()
                ))
            })(),
            Confirm::Unset(file, key) => (|| {
                self.edit_key(&file, &key, None)?;
                Ok(format!("removed key {key}"))
            })(),
            Confirm::Relink(_, _) => unreachable!(),
        };
        self.set_message(result);
    }

    fn edit_external(&mut self) -> Result<String> {
        let file = self.selected_file().ok_or("no file selected")?;
        let project = self.project()?;
        let resolved = self.resolve_selected()?;
        let before = fs::read(&resolved.path).map_err(|e| e.to_string())?;
        let source = &resolved.path;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let draft = source.with_file_name(format!(".envrun-edit-{}-{nonce}", std::process::id()));
        project::write_private_atomic(&draft, &before)?;
        let editor = env::var("VISUAL")
            .or_else(|_| env::var("EDITOR"))
            .unwrap_or_else(|_| "vi".into());
        let mut words = editor.split_whitespace();
        let program = words.next().ok_or("VISUAL or EDITOR is empty")?;
        let status = Command::new(program)
            .args(words)
            .arg(&draft)
            .status()
            .map_err(|e| {
                format!(
                    "cannot launch editor; draft kept at {}: {e}",
                    draft.display()
                )
            })?;
        if !status.success() {
            return Err(format!("editor failed; draft kept at {}", draft.display()));
        }
        let after = fs::read(&draft).map_err(|e| e.to_string())?;
        let saved = if let Some(repo) = &self.repo {
            project::update_file(repo, &file, self.source, &resolved, &before, &after)
        } else {
            project::update_bytes(project, &file, &before, &after)
        };
        if let Err(error) = saved {
            return Err(format!("{error}; draft kept at {}", draft.display()));
        }
        fs::remove_file(&draft).map_err(|e| e.to_string())?;
        Ok(format!("saved {}", file.display()))
    }
}

mod view;
use view::draw;

pub fn run(cwd: &Path) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("the TUI requires an interactive terminal".into());
    }
    let mut app = App::new(cwd)?;
    let mut terminal: DefaultTerminal = ratatui::try_init().map_err(|error| {
        ratatui::restore();
        format!("cannot open terminal UI: {error}")
    })?;
    app.compact = terminal
        .size()
        .map_err(|e| e.to_string())
        .is_ok_and(|size| size.width < 72 || size.height < 22);
    execute!(io::stdout(), EnableMouseCapture).map_err(|error| {
        ratatui::restore();
        format!("cannot enable mouse input: {error}")
    })?;
    let result = (|| -> Result<()> {
        loop {
            let mut areas = HitAreas::default();
            terminal
                .draw(|frame| areas = draw(frame, &app))
                .map_err(|e| e.to_string())?;
            let input = loop {
                let input = event::read().map_err(|e| e.to_string())?;
                if matches!(
                    input,
                    Event::Mouse(MouseEvent {
                        kind: MouseEventKind::Moved
                            | MouseEventKind::Drag(_)
                            | MouseEventKind::Up(_),
                        ..
                    })
                ) {
                    continue;
                }
                break input;
            };
            let outcome = match input {
                Event::Key(key) => app.handle(key),
                Event::Mouse(mouse) => app.handle_mouse(mouse, &areas),
                Event::Resize(width, height) => {
                    app.reveal = false;
                    app.value_scroll = 0;
                    app.compact = width < 72 || height < 22;
                    Outcome::Continue
                }
                _ => Outcome::Continue,
            };
            match outcome {
                Outcome::Continue => {}
                Outcome::Quit => break,
                Outcome::Edit => {
                    let _ = execute!(io::stdout(), DisableMouseCapture);
                    ratatui::restore();
                    let edited = app.edit_external();
                    terminal = ratatui::try_init().map_err(|error| {
                        ratatui::restore();
                        format!("cannot restore terminal UI: {error}")
                    })?;
                    execute!(io::stdout(), EnableMouseCapture)
                        .map_err(|error| format!("cannot enable mouse input: {error}"))?;
                    app.set_message(edited);
                }
            }
        }
        Ok(())
    })();
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyModifiers;

    fn conflict_app(tag: &str) -> (PathBuf, App) {
        let base = std::env::temp_dir().join(format!("envrun-tui-{tag}-{}", std::process::id()));
        let root = base.join("repo");
        let files = base.join("store/files");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&files).unwrap();
        fs::write(root.join(".gitignore"), ".env*\n").unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .arg(&root)
                .status()
                .unwrap()
                .success()
        );
        fs::write(root.join(".env"), "VALUE=local\n").unwrap();
        fs::write(files.join(".env"), "VALUE=shared\n").unwrap();
        let root = fs::canonicalize(root).unwrap();
        let project = Project {
            name: "demo".into(),
            files,
        };
        let repo = Repo {
            root: root.clone(),
            project: project.clone(),
        };
        let mut app = App {
            cwd: root,
            repo: Some(repo),
            project: Some(project),
            available: Vec::new(),
            project_index: 0,
            files: Vec::new(),
            file_index: 0,
            keys: Vec::new(),
            key_index: 0,
            source: Source::Auto,
            source_prefs: BTreeMap::new(),
            managed: BTreeSet::new(),
            file_status: BTreeMap::new(),
            file_labels: BTreeMap::new(),
            values: BTreeMap::new(),
            resolved: None,
            focus_keys: false,
            focus_details: false,
            reveal: false,
            value_scroll: 0,
            help_scroll: 0,
            file_error: None,
            no_color: false,
            compact: false,
            filter: String::new(),
            mode: Mode::Normal,
            message: String::new(),
        };
        app.refresh().unwrap();
        (base, app)
    }

    #[test]
    fn conflicting_sources_are_explicit_and_edits_follow_the_displayed_source() {
        let (base, mut app) = conflict_app("source-choice");
        assert!(app.keys.is_empty());
        assert_eq!(app.selected_status(), "BOTH");
        assert!(
            app.file_error
                .as_deref()
                .unwrap()
                .contains("--source local")
        );
        let shared = app.project().unwrap().files.join(".env");
        let local = app.cwd.join(".env");
        app.handle(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        assert_eq!(app.values["VALUE"], "local");
        assert!(!app.resolved.as_ref().unwrap().shared);
        app.finish_prompt(Prompt::Value("VALUE".into()), "local-edited".into());
        assert_eq!(envfile::get(&local, "VALUE").unwrap(), "local-edited");
        assert_eq!(envfile::get(&shared, "VALUE").unwrap(), "shared");
        app.handle(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        assert_eq!(app.values["VALUE"], "shared");
        assert!(app.resolved.as_ref().unwrap().shared);
        app.finish_prompt(Prompt::Value("VALUE".into()), "shared-edited".into());
        assert_eq!(envfile::get(&local, "VALUE").unwrap(), "local-edited");
        assert_eq!(envfile::get(&shared, "VALUE").unwrap(), "shared-edited");
        assert!(!screen(&app).contains("shared-edited"));
        fs::write(&shared, "VALUE=external-change\n").unwrap();
        app.reveal = true;
        assert!(
            screen(&app).contains("shared-edited"),
            "draw should only use the cached snapshot"
        );
        app.handle(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE));
        assert_eq!(app.values["VALUE"], "external-change");
        assert!(!app.reveal);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn source_choice_survives_navigation_refresh_and_reopen_but_is_worktree_specific() {
        let (base, mut app) = conflict_app("persist-source");
        let repo = app.repo.as_ref().unwrap().clone();
        fs::write(app.cwd.join(".env.local"), "OTHER=local\n").unwrap();
        app.refresh().unwrap();
        app.select_source(Source::Shared);
        assert_eq!(app.source, Source::Shared);
        app.handle(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            app.selected_file().as_deref(),
            Some(Path::new(".env.local"))
        );
        assert_eq!(app.source, Source::Auto);
        app.handle(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.source, Source::Shared);
        app.handle(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE));
        assert_eq!(app.source, Source::Shared);
        assert_eq!(
            project::load_ui_sources(&repo).unwrap()[Path::new(".env")],
            Source::Shared
        );

        let mut reopened = conflict_app("persist-source").1;
        reopened.source_prefs = project::load_ui_sources(&repo).unwrap();
        reopened.refresh().unwrap();
        assert_eq!(reopened.source, Source::Shared);
        let other_root = base.join("other-worktree");
        fs::create_dir(&other_root).unwrap();
        let other_repo = Repo {
            root: other_root,
            project: repo.project.clone(),
        };
        assert!(project::load_ui_sources(&other_repo).unwrap().is_empty());
        reopened.select_source(Source::Auto);
        assert!(project::load_ui_sources(&repo).unwrap().is_empty());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn tui_relink_returns_both_to_linked_and_preserves_divergent_copy() {
        let (base, mut app) = conflict_app("relink");
        let local = app.cwd.join(".env");
        let shared = app.project().unwrap().files.join(".env");
        assert_eq!(app.selected_status(), "BOTH");
        app.handle(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Confirm(Confirm::Relink(_, true))));
        app.handle(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert_eq!(app.selected_status(), "LINKED");
        assert!(local.is_symlink());
        assert_eq!(fs::read_to_string(&shared).unwrap(), "VALUE=shared\n");
        let Mode::Notice(ref notice) = app.mode else {
            panic!("missing backup notice")
        };
        let backup = PathBuf::from(
            notice
                .split_once("saved privately at ")
                .unwrap()
                .1
                .split_once(". Press Esc")
                .unwrap()
                .0,
        );
        assert_eq!(fs::read_to_string(backup).unwrap(), "VALUE=local\n");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn removal_shows_blockers_before_confirmation_and_preserves_local_copy() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;
        let (base, mut app) = conflict_app("remove-guide");
        let shared = app.project().unwrap().files.join(".env");
        let local = app.cwd.join(".env");
        let registrations = base.join("store/repositories");
        fs::create_dir(&registrations).unwrap();
        fs::write(
            registrations.join("a".repeat(32)),
            app.cwd.join(".git").as_os_str().as_bytes(),
        )
        .unwrap();
        fs::remove_file(&local).unwrap();
        symlink(&shared, &local).unwrap();
        app.refresh().unwrap();
        app.handle(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(
            matches!(&app.mode, Mode::RemovalBlocked(_, paths, _) if paths == &vec![app.cwd.clone()])
        );
        app.handle(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(shared.exists());
        app.handle(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        assert!(!local.is_symlink());
        app.handle(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(matches!(&app.mode, Mode::Confirm(Confirm::Remove(_))));
        app.handle(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(!shared.exists());
        assert_eq!(envfile::get(&local, "VALUE").unwrap(), "shared");
        fs::remove_dir_all(base).unwrap();
    }

    fn screen(app: &App) -> String {
        screen_at(app, 100, 24)
    }

    fn screen_at(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, app);
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn responsive_views_show_sources_and_keep_values_masked() {
        let (base, mut app) = conflict_app("responsive");
        app.select_source(Source::Shared);
        for (width, height, divider) in [(140, 40, true), (80, 24, true)] {
            let rendered = screen_at(&app, width, height);
            assert!(rendered.contains("AUTO") || rendered.contains("SHARED"));
            assert!(rendered.contains("KEYS"));
            assert_eq!(rendered.contains('│'), divider);
            assert!(!rendered.contains("VALUE=shared"));
        }
        assert!(screen_at(&app, 72, 22).contains("••••••••"));
        app.compact = true;
        app.focus(Pane::Keys);
        let compact = screen_at(&app, 60, 20);
        assert!(compact.contains("[Keys]"));
        assert!(compact.contains("Value:"));
        assert!(!compact.contains("shared"));
        app.focus(Pane::Details);
        let minimum = screen_at(&app, 50, 16);
        assert!(minimum.contains("Reveal"));
        assert!(minimum.contains("••••••••"));
        assert!(screen_at(&app, 49, 16).contains("Terminal too small"));
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn mouse_source_buttons_select_the_copy_and_hide_revealed_values() {
        let (base, mut app) = conflict_app("source-clicks");
        let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
        let mut hits = HitAreas::default();
        terminal.draw(|frame| hits = draw(frame, &app)).unwrap();
        let (local, _) = hits
            .clicks
            .iter()
            .find(|(_, click)| matches!(click, Click::Source(Source::Local)))
            .unwrap();
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(event::MouseButton::Left),
                column: local.x,
                row: local.y,
                modifiers: KeyModifiers::NONE,
            },
            &hits,
        );
        assert_eq!(app.values["VALUE"], "local");
        app.reveal = true;
        terminal.draw(|frame| hits = draw(frame, &app)).unwrap();
        let (shared, _) = hits
            .clicks
            .iter()
            .find(|(_, click)| matches!(click, Click::Source(Source::Shared)))
            .unwrap();
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(event::MouseButton::Left),
                column: shared.x,
                row: shared.y,
                modifiers: KeyModifiers::NONE,
            },
            &hits,
        );
        assert_eq!(app.values["VALUE"], "shared");
        assert!(!app.reveal);
        app.select_source(Source::Auto);
        app.compact = true;
        app.focus(Pane::Keys);
        let mut narrow = Terminal::new(TestBackend::new(50, 16)).unwrap();
        narrow.draw(|frame| hits = draw(frame, &app)).unwrap();
        let (local, _) = hits
            .clicks
            .iter()
            .find(|(_, click)| matches!(click, Click::Source(Source::Local)))
            .unwrap();
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(event::MouseButton::Left),
                column: local.x,
                row: local.y,
                modifiers: KeyModifiers::NONE,
            },
            &hits,
        );
        assert_eq!(app.values["VALUE"], "local");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn invalid_env_syntax_still_allows_external_repair() {
        let (base, mut app) = conflict_app("repair");
        app.select_source(Source::Local);
        fs::write(app.cwd.join(".env"), "BROKEN=\"unterminated\n").unwrap();
        app.refresh_keys();
        assert!(app.file_error.is_some());
        assert!(app.resolved.is_some());
        assert!(matches!(
            app.handle(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE)),
            Outcome::Edit
        ));
        fs::remove_dir_all(base).unwrap();
    }

    fn areas(app: &App) -> HitAreas {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut areas = HitAreas::default();
        terminal.draw(|frame| areas = draw(frame, app)).unwrap();
        areas
    }

    #[test]
    fn masks_values_and_inline_input_until_explicit_reveal() {
        let base = std::env::temp_dir().join(format!("envrun-tui-test-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join(".env"), "SECRET=private-value\n").unwrap();
        let mut app = App {
            cwd: base.clone(),
            repo: None,
            project: Some(Project {
                name: "demo".into(),
                files: base.clone(),
            }),
            available: Vec::new(),
            project_index: 0,
            files: vec![PathBuf::from(".env")],
            file_index: 0,
            keys: vec!["SECRET".into()],
            key_index: 0,
            source: Source::Auto,
            source_prefs: BTreeMap::new(),
            managed: BTreeSet::new(),
            file_status: BTreeMap::new(),
            file_labels: BTreeMap::new(),
            values: BTreeMap::new(),
            resolved: None,
            focus_keys: true,
            focus_details: false,
            reveal: false,
            value_scroll: 0,
            help_scroll: 0,
            file_error: None,
            no_color: false,
            compact: false,
            filter: String::new(),
            mode: Mode::Normal,
            message: String::new(),
        };
        app.refresh_keys();
        assert!(!screen(&app).contains("private-value"));
        app.mode = Mode::Prompt(Prompt::Value("SECRET".into()), "replacement".into());
        assert!(!screen(&app).contains("replacement"));
        app.mode = Mode::Normal;
        app.reveal = true;
        assert!(screen(&app).contains("private-value"));
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn inline_edit_updates_the_selected_key() {
        let base =
            std::env::temp_dir().join(format!("envrun-tui-edit-test-{}", std::process::id()));
        let files = base.join("files");
        fs::create_dir_all(&files).unwrap();
        fs::write(files.join(".env"), "KEY=old\n").unwrap();
        let project = Project {
            name: "demo".into(),
            files,
        };
        let mut app = App {
            cwd: base.clone(),
            repo: None,
            project: Some(project.clone()),
            available: Vec::new(),
            project_index: 0,
            files: vec![PathBuf::from(".env")],
            file_index: 0,
            keys: vec!["KEY".into()],
            key_index: 0,
            source: Source::Auto,
            source_prefs: BTreeMap::new(),
            managed: BTreeSet::new(),
            file_status: BTreeMap::new(),
            file_labels: BTreeMap::new(),
            values: BTreeMap::new(),
            resolved: None,
            focus_keys: true,
            focus_details: false,
            reveal: false,
            value_scroll: 0,
            help_scroll: 0,
            file_error: None,
            no_color: false,
            compact: false,
            filter: String::new(),
            mode: Mode::Normal,
            message: String::new(),
        };
        app.refresh_keys();
        for code in [
            KeyCode::Char('s'),
            KeyCode::Char('n'),
            KeyCode::Char('e'),
            KeyCode::Char('w'),
            KeyCode::Enter,
        ] {
            app.handle(KeyEvent::new(code, KeyModifiers::NONE));
        }
        assert_eq!(
            project::get_key(&project, Path::new(".env"), "KEY").unwrap(),
            "new"
        );
        assert!(!app.reveal);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn shows_local_keys_before_import() {
        let base =
            std::env::temp_dir().join(format!("envrun-tui-local-test-{}", std::process::id()));
        let root = base.join("repo");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join(".env"),
            "LOCAL_KEY=hidden-value\nSECOND=other-value\n",
        )
        .unwrap();
        fs::write(root.join(".env.local"), "THIRD=local-value\n").unwrap();
        assert!(
            Command::new("git")
                .arg("init")
                .arg("-q")
                .arg(&root)
                .status()
                .unwrap()
                .success()
        );
        let project = Project {
            name: "demo".into(),
            files: base.join("store/files"),
        };
        let repo = Repo {
            root: root.clone(),
            project: project.clone(),
        };
        let mut app = App {
            cwd: root,
            repo: Some(repo),
            project: Some(project),
            available: Vec::new(),
            project_index: 0,
            files: Vec::new(),
            file_index: 0,
            keys: Vec::new(),
            key_index: 0,
            source: Source::Auto,
            source_prefs: BTreeMap::new(),
            managed: BTreeSet::new(),
            file_status: BTreeMap::new(),
            file_labels: BTreeMap::new(),
            values: BTreeMap::new(),
            resolved: None,
            focus_keys: false,
            focus_details: false,
            reveal: false,
            value_scroll: 0,
            help_scroll: 0,
            file_error: None,
            no_color: false,
            compact: false,
            filter: String::new(),
            mode: Mode::Normal,
            message: String::new(),
        };
        app.refresh().unwrap();
        assert_eq!(
            app.files,
            vec![PathBuf::from(".env"), PathBuf::from(".env.local")]
        );
        assert_eq!(app.keys, vec!["LOCAL_KEY", "SECOND"]);
        assert!(!screen(&app).contains("hidden-value"));
        app.handle(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        app.handle(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(app.focus_keys);
        assert_eq!(app.selected_key().as_deref(), Some("SECOND"));
        app.handle(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert!(!app.focus_keys);

        let areas = areas(&app);
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(event::MouseButton::Left),
                column: areas.keys.x + 1,
                row: areas.keys.y,
                modifiers: KeyModifiers::NONE,
            },
            &areas,
        );
        assert!(app.focus_keys);
        assert_eq!(app.selected_key().as_deref(), Some("LOCAL_KEY"));
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: areas.keys.x + 1,
                row: areas.keys.y,
                modifiers: KeyModifiers::NONE,
            },
            &areas,
        );
        assert_eq!(app.selected_key().as_deref(), Some("SECOND"));
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(event::MouseButton::Left),
                column: areas.files.x + 1,
                row: areas.files.y + 1,
                modifiers: KeyModifiers::NONE,
            },
            &areas,
        );
        assert!(!app.focus_keys);
        assert_eq!(
            app.selected_file().as_deref(),
            Some(Path::new(".env.local"))
        );
        assert_eq!(app.keys, vec!["THIRD"]);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn mouse_selects_visible_key_after_list_scrolls() {
        let mut app = App {
            cwd: PathBuf::new(),
            repo: None,
            project: Some(Project {
                name: "demo".into(),
                files: PathBuf::new(),
            }),
            available: Vec::new(),
            project_index: 0,
            files: vec![PathBuf::from(".env")],
            file_index: 0,
            keys: (0..30).map(|index| format!("KEY_{index}")).collect(),
            key_index: 29,
            source: Source::Auto,
            source_prefs: BTreeMap::new(),
            managed: BTreeSet::new(),
            file_status: BTreeMap::new(),
            file_labels: BTreeMap::new(),
            values: BTreeMap::new(),
            resolved: None,
            focus_keys: true,
            focus_details: false,
            reveal: false,
            value_scroll: 0,
            help_scroll: 0,
            file_error: None,
            no_color: false,
            compact: false,
            filter: String::new(),
            mode: Mode::Normal,
            message: String::new(),
        };
        let areas = areas(&app);
        assert!(areas.key_offset > 0);
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(event::MouseButton::Left),
                column: areas.keys.x + 1,
                row: areas.keys.y,
                modifiers: KeyModifiers::NONE,
            },
            &areas,
        );
        assert_eq!(app.key_index, areas.key_offset);
    }

    #[test]
    fn layout_keeps_panes_compact_and_clickable_on_large_terminals() {
        let mut app = App {
            cwd: PathBuf::new(),
            repo: None,
            project: Some(Project {
                name: "demo".into(),
                files: PathBuf::new(),
            }),
            available: Vec::new(),
            project_index: 0,
            files: vec![PathBuf::from(".env")],
            file_index: 0,
            keys: vec!["FIRST".into(), "SECOND".into()],
            key_index: 0,
            source: Source::Auto,
            source_prefs: BTreeMap::new(),
            managed: BTreeSet::new(),
            file_status: BTreeMap::new(),
            file_labels: BTreeMap::new(),
            values: BTreeMap::new(),
            resolved: None,
            focus_keys: false,
            focus_details: false,
            reveal: false,
            value_scroll: 0,
            help_scroll: 0,
            file_error: None,
            no_color: false,
            compact: false,
            filter: String::new(),
            mode: Mode::Normal,
            message: String::new(),
        };
        let mut terminal = Terminal::new(TestBackend::new(180, 60)).unwrap();
        let mut areas = HitAreas::default();
        terminal.draw(|frame| areas = draw(frame, &app)).unwrap();
        assert!(areas.files.x > 0);
        assert!(areas.files.y > 0);
        assert!(areas.keys.right() < 180);
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(event::MouseButton::Left),
                column: areas.keys.x + 2,
                row: areas.keys.y + 1,
                modifiers: KeyModifiers::NONE,
            },
            &areas,
        );
        assert!(app.focus_keys);
        assert_eq!(app.selected_key().as_deref(), Some("SECOND"));
    }
}
