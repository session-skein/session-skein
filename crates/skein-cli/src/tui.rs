//! Keyboard-first standalone Session Skein terminal interface.

use std::collections::VecDeque;
use std::io::IsTerminal;
use std::io::Read;
use std::io::Write;
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::sync::mpsc::TrySendError;
use std::time::Duration;
use std::time::Instant;

use ratatui::DefaultTerminal;
use ratatui::Frame;
use ratatui::crossterm::event;
use ratatui::crossterm::event::DisableBracketedPaste;
use ratatui::crossterm::event::EnableBracketedPaste;
use ratatui::crossterm::event::Event;
use ratatui::crossterm::event::KeyCode;
use ratatui::crossterm::event::KeyEvent;
use ratatui::crossterm::event::KeyEventKind;
use ratatui::crossterm::execute;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Borders;
use ratatui::widgets::List;
use ratatui::widgets::ListItem;
use ratatui::widgets::ListState;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use serde_json::Value;
use skein_core::ControlRun;
use skein_core::ControlRunState;
use skein_core::DaySummary;
use skein_core::ProjectCard;
use skein_core::Registry;
use skein_core::SessionMetadata;
use skein_core::SkeinPaths;
use uuid::Uuid;

use crate::worker_runtime;
use crate::worker_runtime::RedactedWorkerEvent;

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const EVENT_POLL: Duration = Duration::from_millis(100);
const MAX_COMPOSER_BYTES: usize = 64 * 1024;
const MAX_LIVE_EVENTS: usize = 80;
const MAX_CHILD_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_CHILD_DIAGNOSTIC_BYTES: usize = 64 * 1024;
const ACCENT: Color = Color::Rgb(92, 207, 230);
const ACCENT_SOFT: Color = Color::Rgb(78, 154, 166);
const PANEL_FOCUS: Color = Color::Rgb(36, 48, 61);
const MUTED: Color = Color::Rgb(128, 138, 148);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Focus {
    Projects,
    Work,
    Composer,
}

struct DispatchResult {
    request_id: String,
    kind: DispatchKind,
    value: Option<Value>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DispatchKind {
    Dispatched,
    Refused,
    Recovered,
    AuthenticationFailed,
    OutputOversized,
    OutputMalformed,
    ChildUnavailable,
}

enum ConductorInvocation {
    Parsed { success: bool, value: Value },
    AuthenticationFailed,
    OutputOversized,
    OutputMalformed,
}

#[derive(Debug, Eq, PartialEq)]
struct PreparedDispatch {
    prompt: String,
    request_id: String,
}

struct CatalogSnapshot {
    projects: Vec<ProjectCard>,
    sessions: Vec<SessionMetadata>,
    runs: Vec<ControlRun>,
    day_summary: Option<DaySummary>,
}

struct LiveUpdate {
    run_id: i64,
    events: Vec<RedactedWorkerEvent>,
    latest_sequence: u64,
    truncated: bool,
}

struct InterruptResult {
    run_id: i64,
    interrupted: bool,
}

pub fn run(paths: SkeinPaths) -> Result<(), Box<dyn std::error::Error>> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err("skein tui requires an interactive terminal".into());
    }
    let mut app = App::new(paths)?;
    let _paste_guard = BracketedPasteGuard::enable()?;
    ratatui::run(|terminal| app.run(terminal))?;
    Ok(())
}

struct BracketedPasteGuard;

impl BracketedPasteGuard {
    fn enable() -> std::io::Result<Self> {
        execute!(std::io::stdout(), EnableBracketedPaste)?;
        Ok(Self)
    }
}

impl Drop for BracketedPasteGuard {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    }
}

struct App {
    paths: SkeinPaths,
    projects: Vec<ProjectCard>,
    sessions: Vec<SessionMetadata>,
    runs: Vec<ControlRun>,
    day_summary: Option<DaySummary>,
    selected_project: usize,
    selected_work: usize,
    selected_run: Option<i64>,
    pending_run_selection: Option<i64>,
    focus: Focus,
    composer: String,
    full_access_armed: bool,
    status: String,
    should_quit: bool,
    quit_after_dispatch: bool,
    dispatch_rx: Option<mpsc::Receiver<DispatchResult>>,
    dispatching: bool,
    catalog_request: SyncSender<()>,
    catalog_rx: Receiver<Option<CatalogSnapshot>>,
    source_refresh_rx: Receiver<String>,
    refresh_pending: bool,
    live_select: SyncSender<Option<i64>>,
    live_rx: Receiver<LiveUpdate>,
    live_selection_dirty: bool,
    interrupt_rx: Option<Receiver<InterruptResult>>,
    pending_interrupt: Option<i64>,
    live_events: VecDeque<RedactedWorkerEvent>,
    event_run_id: Option<i64>,
    event_sequence: u64,
    last_refresh: Instant,
}

impl App {
    fn new(paths: SkeinPaths) -> Result<Self, Box<dyn std::error::Error>> {
        let initial = load_catalog(&paths)?;
        let (catalog_request, catalog_rx) = spawn_catalog_loader(paths.clone());
        let source_refresh_rx = spawn_source_refresh(paths.clone(), catalog_request.clone());
        let (live_select, live_rx) = spawn_live_poller(paths.clone());
        let mut app = Self {
            paths,
            projects: initial.projects,
            sessions: initial.sessions,
            runs: initial.runs,
            day_summary: initial.day_summary,
            selected_project: 0,
            selected_work: 0,
            selected_run: None,
            pending_run_selection: None,
            focus: Focus::Projects,
            composer: String::new(),
            full_access_armed: false,
            status: "Ready. Refreshing configured sources in the background.".to_owned(),
            should_quit: false,
            quit_after_dispatch: false,
            dispatch_rx: None,
            dispatching: false,
            catalog_request,
            catalog_rx,
            source_refresh_rx,
            refresh_pending: false,
            live_select,
            live_rx,
            live_selection_dirty: false,
            interrupt_rx: None,
            pending_interrupt: None,
            live_events: VecDeque::new(),
            event_run_id: None,
            event_sequence: 0,
            last_refresh: Instant::now(),
        };
        app.clamp_selections();
        Ok(app)
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> std::io::Result<()> {
        while !self.should_quit {
            self.request_periodic_refresh();
            self.poll_catalog();
            self.poll_source_refresh();
            self.poll_dispatch();
            self.poll_interrupt();
            self.flush_live_selection();
            self.poll_live_events();
            terminal.draw(|frame| self.render(frame))?;
            if event::poll(EVENT_POLL)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
                    Event::Paste(text) => self.append_composer(&text),
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn poll_source_refresh(&mut self) {
        if let Ok(status) = self.source_refresh_rx.try_recv() {
            self.status = status;
            let _ = self.catalog_request.try_send(());
        }
    }

    fn apply_catalog(&mut self, snapshot: CatalogSnapshot) {
        let selected_project_id = self.selected_project_id();
        self.projects = snapshot.projects;
        self.sessions = snapshot.sessions;
        self.runs = snapshot.runs;
        self.day_summary = snapshot.day_summary;
        if let Some(project_id) = selected_project_id
            && let Some(index) = self
                .projects
                .iter()
                .position(|card| card.project.id == project_id)
        {
            self.selected_project = index;
        }
        self.clamp_selections();
        if let Some(run_id) = self.pending_run_selection
            && self.runs.iter().any(|run| run.id == run_id)
        {
            self.select_run(run_id);
        }
        self.last_refresh = Instant::now();
    }

    fn clamp_selections(&mut self) {
        if self.projects.is_empty() {
            self.selected_project = 0;
        } else {
            self.selected_project = self.selected_project.min(self.projects.len() - 1);
        }
        self.clamp_work_selection();
        if let Some(run_id) = self.selected_run {
            if let Some(index) = self.project_runs().iter().position(|run| run.id == run_id) {
                self.selected_work = self.project_sessions().len() + index;
            } else {
                self.selected_run = None;
            }
        } else {
            self.sync_selected_work_identity();
        }
    }

    fn request_refresh(&mut self) {
        if self.refresh_pending {
            return;
        }
        match self.catalog_request.try_send(()) {
            Ok(()) => self.refresh_pending = true,
            Err(TrySendError::Full(())) => self.refresh_pending = true,
            Err(TrySendError::Disconnected(())) => {
                self.status = "Catalog loader is unavailable.".to_owned();
            }
        }
    }

    fn request_periodic_refresh(&mut self) {
        if self.last_refresh.elapsed() >= REFRESH_INTERVAL {
            self.request_refresh();
        }
    }

    fn poll_catalog(&mut self) {
        let Ok(snapshot) = self.catalog_rx.try_recv() else {
            return;
        };
        self.refresh_pending = false;
        if let Some(snapshot) = snapshot {
            self.apply_catalog(snapshot);
        } else {
            self.status = "Catalog refresh failed safely; previous view retained.".to_owned();
            self.last_refresh = Instant::now();
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::F(2) => {
                if self.dispatching {
                    self.status = "Wait for the in-flight conductor request.".to_owned();
                    return;
                }
                if self.focus != Focus::Composer {
                    self.status = "Focus the composer before arming full access.".to_owned();
                    return;
                }
                self.full_access_armed = !self.full_access_armed;
                self.status = if self.full_access_armed {
                    "Full access armed for the next conductor dispatch.".to_owned()
                } else {
                    "Full access disarmed.".to_owned()
                };
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Projects => Focus::Work,
                    Focus::Work => Focus::Composer,
                    Focus::Composer => Focus::Projects,
                };
                if self.focus != Focus::Composer {
                    self.full_access_armed = false;
                }
            }
            KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Projects => Focus::Composer,
                    Focus::Work => Focus::Projects,
                    Focus::Composer => Focus::Work,
                };
                if self.focus != Focus::Composer {
                    self.full_access_armed = false;
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('q') if self.focus != Focus::Composer => self.request_quit(),
            KeyCode::Char('r') if self.focus != Focus::Composer => {
                self.request_refresh();
                self.status = "Catalog refresh requested.".to_owned();
            }
            KeyCode::Char('x') if self.focus != Focus::Composer => self.interrupt_selected(),
            KeyCode::Up if self.focus == Focus::Projects => self.previous_project(),
            KeyCode::Down if self.focus == Focus::Projects => self.next_project(),
            KeyCode::Up if self.focus == Focus::Work => self.previous_work(),
            KeyCode::Down if self.focus == Focus::Work => self.next_work(),
            KeyCode::Enter if self.focus == Focus::Composer => self.dispatch(),
            KeyCode::Esc if self.focus == Focus::Composer => {
                self.full_access_armed = false;
                self.focus = Focus::Projects;
            }
            KeyCode::Esc => {
                self.pending_interrupt = None;
                self.status = "Pending action cancelled.".to_owned();
            }
            KeyCode::Backspace if self.focus == Focus::Composer => {
                self.composer.pop();
            }
            KeyCode::Char(character) if self.focus == Focus::Composer => {
                self.append_composer(&character.to_string());
            }
            _ => {}
        }
    }

    fn request_quit(&mut self) {
        if self.dispatching {
            self.quit_after_dispatch = true;
            self.status =
                "Waiting for the conductor handoff; Ctrl-C force-quits without replay.".to_owned();
        } else {
            self.should_quit = true;
        }
    }

    fn append_composer(&mut self, value: &str) {
        if self.focus != Focus::Composer {
            return;
        }
        if self.composer.len().saturating_add(value.len()) <= MAX_COMPOSER_BYTES {
            self.composer.push_str(value);
        } else {
            self.status = "Composer is limited to 64 KiB.".to_owned();
        }
    }

    fn dispatch(&mut self) {
        let Some(prepared) = self.prepare_dispatch() else {
            return;
        };
        let PreparedDispatch { prompt, request_id } = prepared;
        let (sender, receiver) = mpsc::channel();
        self.dispatch_rx = Some(receiver);
        self.status = format!("Routing request {request_id}…");
        let paths = self.paths.clone();
        std::thread::spawn(move || {
            let (kind, value) = classify_invocation(
                invoke_conductor(prompt, request_id.clone()),
                &paths,
                &request_id,
            );
            let _ = sender.send(DispatchResult {
                request_id,
                kind,
                value,
            });
        });
    }

    fn prepare_dispatch(&mut self) -> Option<PreparedDispatch> {
        if self.dispatching {
            self.status = "A conductor request is already being submitted.".to_owned();
            return None;
        }
        if self.composer.trim().is_empty() {
            self.status = "Type a prompt before dispatching.".to_owned();
            return None;
        }
        if !self.full_access_armed {
            self.status = "Press F2 to explicitly arm full access for this dispatch.".to_owned();
            return None;
        }
        let prompt = std::mem::take(&mut self.composer);
        let request_id = Uuid::new_v4().to_string();
        self.dispatching = true;
        self.full_access_armed = false;
        Some(PreparedDispatch { prompt, request_id })
    }

    fn poll_dispatch(&mut self) {
        let received = self
            .dispatch_rx
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        let Some(result) = received else {
            return;
        };
        self.dispatch_rx = None;
        self.dispatching = false;
        let run_id = result.value.as_ref().and_then(extract_run_id);
        self.status = match result.kind {
            DispatchKind::Dispatched => match run_id {
                Some(run_id) => format!(
                    "Request {} dispatched as reconnectable run {run_id}.",
                    result.request_id
                ),
                None => format!("Request {} completed without a run ID.", result.request_id),
            },
            DispatchKind::Refused => format!(
                "Request {} was refused before dispatch; inspect match confidence.",
                result.request_id
            ),
            DispatchKind::Recovered => format!(
                "Request {} recovered durable run {}; prompt replay was disabled.",
                result.request_id,
                run_id.map_or_else(|| "unknown".to_owned(), |id| id.to_string())
            ),
            DispatchKind::AuthenticationFailed => format!(
                "Request {} needs a ChatGPT Codex login; run `codex login`.",
                result.request_id
            ),
            DispatchKind::OutputOversized => format!(
                "Request {} returned oversized conductor output; no durable receipt found.",
                result.request_id
            ),
            DispatchKind::OutputMalformed => format!(
                "Request {} returned malformed conductor output; no durable receipt found.",
                result.request_id
            ),
            DispatchKind::ChildUnavailable => format!(
                "Request {} could not start or finish the conductor child; no durable receipt found.",
                result.request_id
            ),
        };
        if self.quit_after_dispatch {
            self.quit_after_dispatch = false;
            self.should_quit = true;
        }
        self.request_refresh();
        if let Some(run_id) = run_id {
            self.select_run(run_id);
        }
    }

    fn interrupt_selected(&mut self) {
        let Some(run_id) = self.selected_run_id() else {
            self.status = "Select a worker-owned run before interrupting.".to_owned();
            return;
        };
        if self.pending_interrupt != Some(run_id) {
            self.pending_interrupt = Some(run_id);
            self.status = format!("Press x again to interrupt exact run {run_id}; Esc cancels.");
            return;
        }
        if self.interrupt_rx.is_some() {
            self.status = "An interrupt request is already in flight.".to_owned();
            return;
        }
        self.pending_interrupt = None;
        let paths = self.paths.clone();
        let (sender, receiver) = mpsc::channel();
        self.interrupt_rx = Some(receiver);
        self.status = format!("Revalidating and interrupting exact run {run_id}…");
        std::thread::spawn(move || {
            let interrupted = revalidate_and_interrupt(&paths, run_id);
            let _ = sender.send(InterruptResult {
                run_id,
                interrupted,
            });
        });
    }

    fn poll_interrupt(&mut self) {
        let received = self
            .interrupt_rx
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        let Some(result) = received else {
            return;
        };
        self.interrupt_rx = None;
        self.status = if result.interrupted {
            format!("Interrupt queued for exact active run {}.", result.run_id)
        } else {
            format!("Run {} was no longer exactly interruptible.", result.run_id)
        };
        self.request_refresh();
    }

    fn poll_live_events(&mut self) {
        let Ok(update) = self.live_rx.try_recv() else {
            return;
        };
        if self.event_run_id != Some(update.run_id) {
            return;
        }
        self.event_sequence = update.latest_sequence;
        if update.truncated {
            self.live_events.clear();
            let keep_from = update.events.len().saturating_sub(MAX_LIVE_EVENTS - 1);
            let gap_sequence = update
                .events
                .get(keep_from)
                .map_or(update.latest_sequence, |event| {
                    event.sequence.saturating_sub(1)
                });
            self.live_events.push_back(RedactedWorkerEvent {
                sequence: gap_sequence,
                kind: "event_gap".to_owned(),
                thread_id: None,
                turn_id: None,
                status: None,
                item_type: None,
                delta_bytes: None,
            });
            self.live_events
                .extend(update.events.into_iter().skip(keep_from));
        } else {
            self.live_events.extend(update.events);
        }
        while self.live_events.len() > MAX_LIVE_EVENTS {
            self.live_events.pop_front();
        }
    }

    fn reset_events(&mut self, run_id: Option<i64>) {
        self.event_run_id = run_id;
        self.event_sequence = 0;
        self.live_events.clear();
    }

    fn selected_project_id(&self) -> Option<i64> {
        self.projects
            .get(self.selected_project)
            .map(|card| card.project.id)
    }

    fn project_sessions(&self) -> Vec<&SessionMetadata> {
        let project_id = self.selected_project_id();
        self.sessions
            .iter()
            .filter(|session| session.project_id == project_id)
            .collect()
    }

    fn project_runs(&self) -> Vec<&ControlRun> {
        let project_id = self.selected_project_id();
        self.runs
            .iter()
            .filter(|run| Some(run.project_id) == project_id)
            .collect()
    }

    fn work_len(&self) -> usize {
        self.project_sessions().len() + self.project_runs().len()
    }

    fn selected_run_id(&self) -> Option<i64> {
        self.selected_run
    }

    fn selected_session(&self) -> Option<&SessionMetadata> {
        let sessions = self.project_sessions();
        sessions.get(self.selected_work).copied()
    }

    fn blocker_message(&self) -> Option<String> {
        if let Some(run_id) = self.selected_run
            && let Some(run) = self.runs.iter().find(|run| run.id == run_id)
        {
            match run.state {
                ControlRunState::RecoveryRequired => {
                    return Some(format!(
                        "BLOCKER run #{run_id}: worker ownership was lost; read/reconcile before resuming"
                    ));
                }
                ControlRunState::Failed => {
                    return Some(format!(
                        "BLOCKER run #{run_id}: failed; inspect durable control status before retrying"
                    ));
                }
                _ => {}
            }
        }
        if let Some(session) = self.selected_session() {
            let matching_run = |run: &&ControlRun| {
                session.source_kind.eq_ignore_ascii_case("codex")
                    && Some(run.project_id) == session.project_id
                    && run.source_thread_id.as_deref() == Some(session.source_thread_id.as_str())
            };
            if let Some(run) = self
                .runs
                .iter()
                .filter(matching_run)
                .find(|run| run.state == ControlRunState::RecoveryRequired)
            {
                return Some(format!(
                    "BLOCKER run #{}: worker ownership was lost; read/reconcile before resuming",
                    run.id
                ));
            }
            if session.ephemeral {
                return Some(
                    "BLOCKER selected thread is ephemeral; start a new durable thread".to_owned(),
                );
            }
            if session
                .observed_status_label
                .eq_ignore_ascii_case("systemError")
            {
                return Some(
                    "BLOCKER last source observation reports a system error; refresh/inspect Codex before resuming"
                        .to_owned(),
                );
            }
            if session.observed_status_label.eq_ignore_ascii_case("active") {
                let controlled_here = self.runs.iter().filter(matching_run).any(|run| {
                    matches!(
                        run.state,
                        ControlRunState::Planned
                            | ControlRunState::Starting
                            | ControlRunState::Active
                    )
                });
                if !controlled_here {
                    return Some(
                        "BLOCKER last source observation says active with no matching Skein owner; refresh/inspect Codex"
                            .to_owned(),
                    );
                }
            }
        }
        self.projects
            .get(self.selected_project)
            .filter(|card| card.facts.recovery_runs > 0)
            .map(|card| {
                format!(
                    "BLOCKER {} run(s) need recovery; select one and use read/reconcile",
                    card.facts.recovery_runs
                )
            })
    }

    fn select_run(&mut self, run_id: i64) {
        self.pending_run_selection = Some(run_id);
        let Some(run) = self.runs.iter().find(|run| run.id == run_id) else {
            return;
        };
        if let Some(project_index) = self
            .projects
            .iter()
            .position(|card| card.project.id == run.project_id)
        {
            self.selected_project = project_index;
            let session_count = self.project_sessions().len();
            if let Some(run_index) = self
                .project_runs()
                .iter()
                .position(|item| item.id == run_id)
            {
                self.selected_work = session_count + run_index;
                self.selected_run = Some(run_id);
                self.pending_run_selection = None;
                self.set_live_run(Some(run_id));
                self.focus = Focus::Work;
            }
        }
    }

    fn next_project(&mut self) {
        self.pending_interrupt = None;
        if !self.projects.is_empty() {
            self.selected_project = (self.selected_project + 1).min(self.projects.len() - 1);
            self.selected_work = 0;
            self.selected_run = None;
            self.set_live_run(None);
        }
    }

    fn previous_project(&mut self) {
        self.pending_interrupt = None;
        self.selected_project = self.selected_project.saturating_sub(1);
        self.selected_work = 0;
        self.selected_run = None;
        self.set_live_run(None);
    }

    fn next_work(&mut self) {
        self.pending_interrupt = None;
        let len = self.work_len();
        if len > 0 {
            self.selected_work = (self.selected_work + 1).min(len - 1);
            self.sync_selected_work_identity();
        }
    }

    fn previous_work(&mut self) {
        self.pending_interrupt = None;
        self.selected_work = self.selected_work.saturating_sub(1);
        self.sync_selected_work_identity();
    }

    fn clamp_work_selection(&mut self) {
        let len = self.work_len();
        self.selected_work = if len == 0 {
            0
        } else {
            self.selected_work.min(len - 1)
        };
    }

    fn sync_selected_work_identity(&mut self) {
        let session_count = self.project_sessions().len();
        let run_id = self
            .selected_work
            .checked_sub(session_count)
            .and_then(|index| self.project_runs().get(index).map(|run| run.id));
        self.selected_run = run_id;
        self.set_live_run(run_id);
    }

    fn set_live_run(&mut self, run_id: Option<i64>) {
        if self.event_run_id == run_id {
            return;
        }
        self.reset_events(run_id);
        self.live_selection_dirty = !matches!(self.live_select.try_send(run_id), Ok(()));
    }

    fn flush_live_selection(&mut self) {
        if !self.live_selection_dirty {
            return;
        }
        match self.live_select.try_send(self.event_run_id) {
            Ok(()) => self.live_selection_dirty = false,
            Err(TrySendError::Disconnected(_)) => self.live_selection_dirty = false,
            Err(TrySendError::Full(_)) => {}
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(10),
                Constraint::Length(5),
                Constraint::Length(1),
            ])
            .split(area);
        self.render_header(frame, outer[0]);
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(32), Constraint::Percentage(68)])
            .split(outer[1]);
        self.render_projects(frame, body[0]);
        let detail = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(body[1]);
        self.render_work(frame, detail[0]);
        self.render_activity(frame, detail[1]);
        self.render_composer(frame, outer[2]);
        self.render_footer(frame, outer[3]);
    }

    fn render_header(&self, frame: &mut Frame<'_>, area: Rect) {
        let active = self
            .runs
            .iter()
            .filter(|run| {
                matches!(
                    run.state,
                    ControlRunState::Planned
                        | ControlRunState::Starting
                        | ControlRunState::Active
                        | ControlRunState::RecoveryRequired
                )
            })
            .count();
        let line = Line::from(vec![
            Span::styled(
                " ≋ SESSION SKEIN ",
                Style::default()
                    .fg(Color::Black)
                    .bg(ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {} projects", self.projects.len()),
                Style::default().fg(Color::White),
            ),
            Span::styled(
                format!("  •  {} sessions", self.sessions.len()),
                Style::default().fg(MUTED),
            ),
            Span::styled(
                format!("  •  {active} open/recovery"),
                Style::default().fg(if active == 0 { MUTED } else { Color::Yellow }),
            ),
            Span::styled(
                if self.dispatching {
                    "  ◐ working"
                } else {
                    "  ● ready"
                },
                Style::default().fg(if self.dispatching {
                    Color::Yellow
                } else {
                    Color::Green
                }),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(line).block(Block::default().borders(Borders::BOTTOM)),
            area,
        );
    }

    fn render_projects(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let items = self
            .projects
            .iter()
            .map(|card| {
                ListItem::new(vec![
                    Line::styled(
                        card.title.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Line::from(format!(
                        "{} threads • {} runs",
                        card.facts.linked_sessions, card.facts.control_runs
                    )),
                ])
            })
            .collect::<Vec<_>>();
        let title = focus_title(" Projects ", self.focus == Focus::Projects);
        let list = List::new(items)
            .block(panel(title, self.focus == Focus::Projects))
            .highlight_style(Style::default().bg(PANEL_FOCUS).fg(Color::White))
            .highlight_symbol("▸ ");
        let mut state = ListState::default()
            .with_selected((!self.projects.is_empty()).then_some(self.selected_project));
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn render_work(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let mut items = Vec::new();
        for session in self.project_sessions() {
            items.push(ListItem::new(Line::from(vec![
                Span::styled("thread ", Style::default().fg(Color::Blue)),
                Span::raw(short_id(&session.source_thread_id)),
                Span::raw(format!("  {}", session.observed_status_label)),
            ])));
        }
        for run in self.project_runs() {
            items.push(ListItem::new(Line::from(vec![
                Span::styled("run ", Style::default().fg(run_color(run.state))),
                Span::raw(format!("#{}  {:?}", run.id, run.state)),
                Span::raw(
                    run.source_thread_id
                        .as_deref()
                        .map(|id| format!("  {}", short_id(id)))
                        .unwrap_or_default(),
                ),
            ])));
        }
        let title = self
            .projects
            .get(self.selected_project)
            .map_or_else(|| " Work ".to_owned(), |card| format!(" {} ", card.title));
        let list = List::new(items)
            .block(panel(
                focus_title(&title, self.focus == Focus::Work),
                self.focus == Focus::Work,
            ))
            .highlight_style(Style::default().bg(PANEL_FOCUS).fg(Color::White))
            .highlight_symbol("▸ ");
        let len = self.work_len();
        let mut state = ListState::default().with_selected((len > 0).then_some(self.selected_work));
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn render_activity(&self, frame: &mut Frame<'_>, area: Rect) {
        let activity_area = if let Some(blocker) = self.blocker_message() {
            let sections = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(area.height.min(3)), Constraint::Min(0)])
                .split(area);
            frame.render_widget(
                Paragraph::new(blocker)
                    .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
                    .wrap(Wrap { trim: true }),
                sections[0],
            );
            sections[1]
        } else {
            area
        };
        let lines = if self.live_events.is_empty() {
            let mut lines = Vec::new();
            if let Some(day) = &self.day_summary {
                lines.push(Line::styled(
                    format!("Today • {}", day.date),
                    Style::default().fg(Color::Cyan),
                ));
                lines.push(Line::from(day.narrative.clone()));
                lines.push(Line::from(""));
            }
            lines.extend(
                self.projects
                    .get(self.selected_project)
                    .map(|card| vec![Line::from(card.narrative.clone())])
                    .unwrap_or_else(|| vec![Line::from("Register a project to begin.")]),
            );
            lines
        } else {
            self.live_events
                .iter()
                .map(|event| {
                    Line::from(format!(
                        "{:>4}  {}{}{}",
                        event.sequence,
                        event.kind,
                        event
                            .status
                            .as_deref()
                            .map(|status| format!(" [{status}]"))
                            .unwrap_or_default(),
                        event
                            .delta_bytes
                            .map(|bytes| format!(" +{bytes}B"))
                            .unwrap_or_default()
                    ))
                })
                .collect()
        };
        let activity_title = if self
            .live_events
            .iter()
            .any(|event| event.kind == "event_gap")
        {
            " Activity / live redacted events • history gap "
        } else {
            " Activity / live redacted events "
        };
        let block = panel(Line::from(activity_title), false);
        let inner = block.inner(activity_area);
        let mut paragraph = Paragraph::new(lines).block(block);
        if !self.live_events.is_empty() && inner.height > 0 {
            let scroll = self.live_events.len().saturating_sub(inner.height as usize);
            paragraph = paragraph.scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0));
        } else {
            paragraph = paragraph.wrap(Wrap { trim: true });
        }
        frame.render_widget(paragraph, activity_area);
    }

    fn render_composer(&self, frame: &mut Frame<'_>, area: Rect) {
        let policy = if self.full_access_armed {
            Span::styled(" ARMED ", Style::default().fg(Color::Black).bg(Color::Red))
        } else {
            Span::styled(" disarmed ", Style::default().fg(Color::DarkGray))
        };
        let title = Line::from(vec![
            Span::raw(" Global conductor composer • full access "),
            policy,
        ]);
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if self.focus == Focus::Composer {
                Style::default().fg(ACCENT)
            } else {
                Style::default()
            });
        let inner = block.inner(area);
        frame.render_widget(
            Paragraph::new(self.composer.as_str())
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
        if self.focus == Focus::Composer && inner.width > 0 && inner.height > 0 {
            let width = inner.width;
            let char_count = self.composer.chars().count() as u16;
            let x = inner.x + (char_count % width);
            let y = inner.y + (char_count / width).min(inner.height.saturating_sub(1));
            frame.set_cursor_position((x, y));
        }
    }

    fn render_footer(&self, frame: &mut Frame<'_>, area: Rect) {
        let keys = "Tab focus  ↑↓ select  F2 arm  Enter dispatch  x interrupt  r refresh  q quit";
        let status = if self.dispatching {
            format!("{}  •  dispatching", self.status)
        } else {
            self.status.clone()
        };
        let available = area.width as usize;
        let text = if status.len() + keys.len() + 3 <= available {
            format!("{status}  │  {keys}")
        } else {
            status
        };
        frame.render_widget(Paragraph::new(text).style(Style::default().fg(MUTED)), area);
    }
}

fn load_catalog(paths: &SkeinPaths) -> Result<CatalogSnapshot, Box<dyn std::error::Error>> {
    let registry = Registry::open_read_only(paths)?;
    Ok(CatalogSnapshot {
        projects: registry.project_cards()?,
        sessions: registry.list_session_metadata()?,
        runs: registry.list_control_runs()?,
        day_summary: crate::local_day_bounds(None).ok().and_then(
            |(date, timezone, start_at, end_at)| {
                registry
                    .day_summary(&date, &timezone, start_at, end_at)
                    .ok()
            },
        ),
    })
}

fn spawn_catalog_loader(paths: SkeinPaths) -> (SyncSender<()>, Receiver<Option<CatalogSnapshot>>) {
    let (request_tx, request_rx) = mpsc::sync_channel::<()>(1);
    let (result_tx, result_rx) = mpsc::sync_channel::<Option<CatalogSnapshot>>(1);
    std::thread::spawn(move || {
        while request_rx.recv().is_ok() {
            let snapshot = load_catalog(&paths).ok();
            if result_tx.send(snapshot).is_err() {
                break;
            }
        }
    });
    (request_tx, result_rx)
}

fn spawn_source_refresh(paths: SkeinPaths, catalog_request: SyncSender<()>) -> Receiver<String> {
    let (status_tx, status_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let status = match Registry::open(&paths) {
            Ok(mut registry) => {
                let discovery = registry.discover_all_scan_roots();
                let git = crate::refresh_git_resilient(&registry, false, false);
                let documents = crate::refresh_documents_resilient(&mut registry);
                let context = crate::codex_home(None)
                    .map_err(|error| error.to_string())
                    .and_then(|home| {
                        registry
                            .refresh_context_documents(
                                &home,
                                skein_core::ContextDocumentRefreshOptions::default(),
                            )
                            .map_err(|error| error.to_string())
                    });
                let sessions =
                    crate::sync_codex_catalog_default(&paths).map_err(|error| error.to_string());
                match (discovery, git, documents, context, sessions) {
                    (Ok(discovery), Ok(git), Ok(documents), Ok(context), Ok(sessions)) => {
                        let repositories = discovery
                            .iter()
                            .map(|report| report.discovered.len())
                            .sum::<usize>();
                        let errors = discovery
                            .iter()
                            .map(|report| report.errors.len())
                            .sum::<usize>();
                        let git_errors = git.iter().filter(|report| report["ok"] == false).count();
                        let document_errors = documents
                            .iter()
                            .filter(|report| report["ok"] == false)
                            .count();
                        format!(
                            "Sources refreshed: {repositories} discovered • {} Git ({} unavailable) • {} documents ({} unavailable) • {} context • {} sessions • {errors} scan errors",
                            git.len().saturating_sub(git_errors),
                            git_errors,
                            documents.len().saturating_sub(document_errors),
                            document_errors,
                            context.memories.documents + context.sessions.documents,
                            sessions.source_threads_selected
                        )
                    }
                    (discovery, git, documents, context, sessions) => format!(
                        "Source refresh incomplete: discovery={} Git={} documents={} context={} sessions={}",
                        result_label(&discovery),
                        result_label(&git),
                        result_label(&documents),
                        result_label(&context),
                        result_label(&sessions)
                    ),
                }
            }
            Err(error) => format!("Source refresh unavailable: {error}"),
        };
        let _ = status_tx.send(status);
        let _ = catalog_request.try_send(());
    });
    status_rx
}

fn result_label<T, E>(result: &Result<T, E>) -> &'static str {
    if result.is_ok() { "ok" } else { "failed" }
}

fn spawn_live_poller(paths: SkeinPaths) -> (SyncSender<Option<i64>>, Receiver<LiveUpdate>) {
    let (select_tx, select_rx) = mpsc::sync_channel::<Option<i64>>(1);
    let (update_tx, update_rx) = mpsc::sync_channel::<LiveUpdate>(1);
    std::thread::spawn(move || {
        let mut selected = None;
        let mut cursors = std::collections::HashMap::<i64, u64>::new();
        loop {
            match select_rx.recv_timeout(Duration::from_millis(300)) {
                Ok(run_id) => selected = run_id,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            while let Ok(run_id) = select_rx.try_recv() {
                selected = run_id;
            }
            let Some(run_id) = selected else {
                continue;
            };
            let after = *cursors.get(&run_id).unwrap_or(&0);
            let Ok(snapshot) = worker_runtime::snapshot(&paths, run_id, after) else {
                continue;
            };
            let latest_sequence = snapshot.latest_sequence;
            let update = LiveUpdate {
                run_id,
                events: snapshot.events,
                latest_sequence,
                truncated: snapshot.events_truncated,
            };
            match update_tx.try_send(update) {
                Ok(()) => {
                    cursors.insert(run_id, latest_sequence);
                }
                Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
    });
    (select_tx, update_rx)
}

fn revalidate_and_interrupt(paths: &SkeinPaths, run_id: i64) -> bool {
    let eligible = Registry::open_read_only(paths)
        .and_then(|registry| registry.control_run(run_id))
        .ok()
        .flatten()
        .is_some_and(|run| run.ownership_mode == "worker" && run.state == ControlRunState::Active);
    eligible && worker_runtime::interrupt(paths, run_id).is_ok()
}

fn invoke_conductor(
    prompt: String,
    request_id: String,
) -> Result<ConductorInvocation, Box<dyn std::error::Error + Send + Sync>> {
    let executable = std::env::current_exe()?;
    let mut child = Command::new(executable)
        .args([
            "conduct",
            "--full-access",
            "--request-id",
            &request_id,
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or("conductor stdout was unavailable")?;
    let stdout_reader = std::thread::spawn(move || {
        let mut body = Vec::new();
        stdout
            .take((MAX_CHILD_OUTPUT_BYTES + 1) as u64)
            .read_to_end(&mut body)
            .map(|_| body)
    });
    let stderr = child
        .stderr
        .take()
        .ok_or("conductor stderr was unavailable")?;
    let stderr_reader = std::thread::spawn(move || {
        let mut body = Vec::new();
        stderr
            .take((MAX_CHILD_DIAGNOSTIC_BYTES + 1) as u64)
            .read_to_end(&mut body)
            .map(|_| body)
    });
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or("conductor stdin was unavailable")?;
        stdin.write_all(prompt.as_bytes())?;
    }
    let status = child.wait()?;
    let body = stdout_reader
        .join()
        .map_err(|_| "conductor stdout reader failed")??;
    let diagnostic = stderr_reader
        .join()
        .map_err(|_| "conductor stderr reader failed")??;
    if body.len() > MAX_CHILD_OUTPUT_BYTES {
        return Ok(ConductorInvocation::OutputOversized);
    }
    match serde_json::from_slice::<Value>(&body) {
        Ok(value) => Ok(ConductorInvocation::Parsed {
            success: status.success(),
            value,
        }),
        Err(_)
            if diagnostic
                .windows(b"Codex authentication is required".len())
                .any(|window| window == b"Codex authentication is required") =>
        {
            Ok(ConductorInvocation::AuthenticationFailed)
        }
        Err(_) => Ok(ConductorInvocation::OutputMalformed),
    }
}

fn classify_invocation(
    invocation: Result<ConductorInvocation, Box<dyn std::error::Error + Send + Sync>>,
    paths: &SkeinPaths,
    request_id: &str,
) -> (DispatchKind, Option<Value>) {
    match invocation {
        Ok(ConductorInvocation::Parsed { success, value }) => {
            let kind = if success {
                DispatchKind::Dispatched
            } else if extract_run_id(&value).is_some() {
                DispatchKind::Recovered
            } else {
                DispatchKind::Refused
            };
            (kind, Some(value))
        }
        other => {
            let recovered = reconcile_request(paths, request_id);
            if recovered.is_some() {
                return (DispatchKind::Recovered, recovered);
            }
            let kind = match other {
                Ok(ConductorInvocation::AuthenticationFailed) => DispatchKind::AuthenticationFailed,
                Ok(ConductorInvocation::OutputOversized) => DispatchKind::OutputOversized,
                Ok(ConductorInvocation::OutputMalformed) => DispatchKind::OutputMalformed,
                Ok(ConductorInvocation::Parsed { .. }) => unreachable!(),
                Err(_) => DispatchKind::ChildUnavailable,
            };
            (kind, None)
        }
    }
}

fn reconcile_request(paths: &SkeinPaths, request_id: &str) -> Option<Value> {
    let registry = Registry::open_read_only(paths).ok()?;
    let decision = registry
        .conductor_decision_by_request_id(request_id)
        .ok()??;
    let run = registry.control_run(decision.run_id).ok()??;
    let worker = registry.control_worker(decision.run_id).ok()?;
    Some(serde_json::json!({
        "requestId": request_id,
        "reused": true,
        "dispatched": false,
        "decision": decision,
        "run": run,
        "worker": worker,
        "responseRecovered": true
    }))
}

fn extract_run_id(value: &Value) -> Option<i64> {
    value["snapshot"]["run"]["id"]
        .as_i64()
        .or_else(|| value["run"]["id"].as_i64())
        .or_else(|| value["decision"]["runId"].as_i64())
}

fn focus_title<'a>(title: &'a str, focused: bool) -> Line<'a> {
    if focused {
        Line::styled(
            title,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )
    } else {
        Line::from(title)
    }
}

fn panel<'a>(title: Line<'a>, focused: bool) -> Block<'a> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused { ACCENT } else { ACCENT_SOFT }))
}

fn short_id(value: &str) -> String {
    value.chars().take(12).collect()
}

const fn run_color(state: ControlRunState) -> Color {
    match state {
        ControlRunState::Completed => Color::Green,
        ControlRunState::Failed | ControlRunState::RecoveryRequired => Color::Red,
        ControlRunState::Interrupted => Color::Yellow,
        ControlRunState::Planned | ControlRunState::Starting | ControlRunState::Active => {
            Color::Cyan
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use skein_core::Project;
    use skein_core::ProjectCardFacts;
    use skein_core::SessionObservation;

    use super::*;

    fn empty_app() -> App {
        let (catalog_request, _catalog_requests) = mpsc::sync_channel(1);
        let (_catalog_results, catalog_rx) = mpsc::sync_channel(1);
        let (_source_status, source_refresh_rx) = mpsc::channel();
        let (live_select, _live_selections) = mpsc::sync_channel(1);
        let (_live_updates, live_rx) = mpsc::sync_channel(1);
        App {
            paths: SkeinPaths::new(PathBuf::from("config"), PathBuf::from("data")),
            projects: Vec::new(),
            sessions: Vec::new(),
            runs: Vec::new(),
            day_summary: None,
            selected_project: 0,
            selected_work: 0,
            selected_run: None,
            pending_run_selection: None,
            focus: Focus::Projects,
            composer: String::new(),
            full_access_armed: false,
            status: "Ready".to_owned(),
            should_quit: false,
            quit_after_dispatch: false,
            dispatch_rx: None,
            dispatching: false,
            catalog_request,
            catalog_rx,
            source_refresh_rx,
            refresh_pending: false,
            live_select,
            live_rx,
            live_selection_dirty: false,
            interrupt_rx: None,
            pending_interrupt: None,
            live_events: VecDeque::new(),
            event_run_id: None,
            event_sequence: 0,
            last_refresh: Instant::now(),
        }
    }

    #[test]
    fn keyboard_focus_and_single_use_policy_are_explicit() {
        let mut app = empty_app();
        app.handle_key(KeyEvent::new(KeyCode::F(2), event::KeyModifiers::NONE));
        assert!(!app.full_access_armed);
        assert!(app.status.contains("Focus"));
        app.handle_key(KeyEvent::new(KeyCode::Tab, event::KeyModifiers::NONE));
        assert_eq!(app.focus, Focus::Work);
        app.handle_key(KeyEvent::new(KeyCode::Tab, event::KeyModifiers::NONE));
        assert_eq!(app.focus, Focus::Composer);
        app.handle_key(KeyEvent::new(KeyCode::Char('h'), event::KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('i'), event::KeyModifiers::NONE));
        assert_eq!(app.composer, "hi");
        app.handle_key(KeyEvent::new(KeyCode::F(2), event::KeyModifiers::NONE));
        assert!(app.full_access_armed);
        let prepared = app.prepare_dispatch().expect("armed request");
        assert_eq!(prepared.prompt, "hi");
        assert!(Uuid::parse_str(&prepared.request_id).is_ok());
        assert!(!app.full_access_armed);
        assert!(app.composer.is_empty());
        assert!(app.dispatching);
        assert!(app.prepare_dispatch().is_none());

        app.dispatching = false;
        app.composer = "second".to_owned();
        assert!(app.prepare_dispatch().is_none());
        assert!(app.status.contains("F2"));
    }

    #[test]
    fn composer_accepts_command_keys_and_rejects_oversized_paste_atomically() {
        let mut app = empty_app();
        app.focus = Focus::Composer;
        for character in ['q', 'r', 'x'] {
            app.handle_key(KeyEvent::new(
                KeyCode::Char(character),
                event::KeyModifiers::NONE,
            ));
        }
        assert_eq!(app.composer, "qrx");
        assert!(!app.should_quit);

        let oversized = "a".repeat(MAX_COMPOSER_BYTES);
        app.append_composer(&oversized);
        assert_eq!(app.composer, "qrx");
        assert!(app.status.contains("64 KiB"));
    }

    #[test]
    fn normal_quit_waits_for_handoff_but_control_c_is_force_quit() {
        let mut app = empty_app();
        app.dispatching = true;
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), event::KeyModifiers::NONE));
        assert!(!app.should_quit);
        assert!(app.status.contains("handoff"));
        assert!(app.quit_after_dispatch);

        app.handle_key(KeyEvent::new(
            KeyCode::Char('c'),
            event::KeyModifiers::CONTROL,
        ));
        assert!(app.should_quit);
    }

    #[test]
    fn pending_normal_quit_completes_after_handoff_result() {
        let mut app = empty_app();
        app.dispatching = true;
        app.request_quit();
        let (sender, receiver) = mpsc::channel();
        app.dispatch_rx = Some(receiver);
        sender
            .send(DispatchResult {
                request_id: "synthetic-request".to_owned(),
                kind: DispatchKind::Dispatched,
                value: Some(serde_json::json!({"run": {"id": 9}})),
            })
            .expect("dispatch result");
        app.poll_dispatch();
        assert!(app.should_quit);
    }

    #[test]
    fn child_failure_classes_are_not_reported_as_route_refusals() {
        let paths = SkeinPaths::new(
            PathBuf::from("missing-config"),
            PathBuf::from("missing-data"),
        );
        for (invocation, expected) in [
            (
                ConductorInvocation::AuthenticationFailed,
                DispatchKind::AuthenticationFailed,
            ),
            (
                ConductorInvocation::OutputOversized,
                DispatchKind::OutputOversized,
            ),
            (
                ConductorInvocation::OutputMalformed,
                DispatchKind::OutputMalformed,
            ),
        ] {
            let (kind, value) = classify_invocation(Ok(invocation), &paths, "synthetic-request");
            assert_eq!(kind, expected);
            assert!(value.is_none());
        }
        let (kind, _) = classify_invocation(
            Ok(ConductorInvocation::Parsed {
                success: false,
                value: serde_json::json!({"recommendation": null}),
            }),
            &paths,
            "synthetic-request",
        );
        assert_eq!(kind, DispatchKind::Refused);
    }

    #[test]
    fn truncated_live_events_insert_an_ordered_gap_and_render_the_newest() {
        let mut app = empty_app();
        let (sender, receiver) = mpsc::channel();
        app.live_rx = receiver;
        app.reset_events(Some(9));
        let events = (5..=104)
            .map(|sequence| redacted_event(sequence, &format!("event-{sequence}")))
            .collect::<Vec<_>>();
        sender
            .send(LiveUpdate {
                run_id: 9,
                events,
                latest_sequence: 104,
                truncated: true,
            })
            .expect("live update");
        app.poll_live_events();
        assert_eq!(app.live_events.len(), MAX_LIVE_EVENTS);
        assert_eq!(
            app.live_events.front().map(|event| event.kind.as_str()),
            Some("event_gap")
        );
        assert_eq!(
            app.live_events.front().map(|event| event.sequence),
            Some(25)
        );
        assert_eq!(
            app.live_events.back().map(|event| event.sequence),
            Some(104)
        );

        let backend = TestBackend::new(100, 28);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| app.render(frame)).expect("render");
        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("event-104"));
        assert!(!rendered.contains("event-26"));
        assert!(rendered.contains("history gap"));
    }

    #[test]
    fn catalog_refresh_preserves_selected_project_identity() {
        let mut app = empty_app();
        app.projects = vec![project_card(1, "first"), project_card(2, "second")];
        app.selected_project = 1;
        app.apply_catalog(CatalogSnapshot {
            projects: vec![project_card(2, "second"), project_card(1, "first")],
            sessions: Vec::new(),
            runs: Vec::new(),
            day_summary: None,
        });
        assert_eq!(app.selected_project_id(), Some(2));
    }

    #[test]
    fn newly_dispatched_run_is_selected_after_catalog_refresh() {
        let mut app = empty_app();
        app.projects = vec![project_card(1, "first")];
        app.select_run(9);
        assert_eq!(app.pending_run_selection, Some(9));

        app.apply_catalog(CatalogSnapshot {
            projects: vec![project_card(1, "first")],
            sessions: Vec::new(),
            runs: vec![control_run(9, 1)],
            day_summary: None,
        });
        assert_eq!(app.selected_run, Some(9));
        assert_eq!(app.pending_run_selection, None);
        assert_eq!(app.focus, Focus::Work);
    }

    #[test]
    fn catalog_loader_never_retains_opted_in_session_text() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let mut registry = Registry::open(&paths).expect("registry");
        let sentinel = "PRIVATE-TUI-SENTINEL";
        registry
            .import_sessions(&[SessionObservation {
                source_kind: "codex".to_owned(),
                source_thread_id: "synthetic-thread".to_owned(),
                source_session_id: Some("synthetic-session".to_owned()),
                source_cwd: temp.path().to_path_buf(),
                source_created_at: 1,
                source_updated_at: 2,
                source_label: "cli".to_owned(),
                observed_status_label: "idle".to_owned(),
                model_provider: None,
                source_version: None,
                parent_source_thread_id: None,
                forked_from_source_thread_id: None,
                ephemeral: false,
                name: Some(sentinel.to_owned()),
                preview: Some(sentinel.to_owned()),
                text_imported: true,
            }])
            .expect("session import");
        drop(registry);

        let snapshot = load_catalog(&paths).expect("catalog");
        assert_eq!(snapshot.sessions.len(), 1);
        assert!(!format!("{:?}", snapshot.sessions).contains(sentinel));
    }

    #[test]
    fn actionable_blockers_are_pinned_from_durable_state() {
        let mut app = empty_app();
        let mut card = project_card(1, "first");
        card.facts.recovery_runs = 2;
        app.projects = vec![card];
        assert!(
            app.blocker_message().is_some_and(|message| {
                message.contains("need recovery") && message.contains('2')
            })
        );

        app.runs = vec![control_run(9, 1)];
        app.runs[0].state = ControlRunState::RecoveryRequired;
        app.select_run(9);
        assert!(
            app.blocker_message()
                .is_some_and(|message| message.contains("read/reconcile"))
        );

        app.selected_run = None;
        app.selected_work = 0;
        app.sessions = vec![session_metadata(1, true, "idle")];
        assert!(
            app.blocker_message()
                .is_some_and(|message| message.contains("ephemeral"))
        );

        app.sessions.clear();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| app.render(frame)).expect("render");
        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("BLOCKER"));
        assert!(rendered.contains("read/reconcile"));
    }

    #[test]
    fn active_source_observation_is_cautious_and_correlated_with_owned_runs() {
        let mut app = empty_app();
        app.projects = vec![project_card(1, "first")];
        app.sessions = vec![session_metadata(1, false, "active")];
        let message = app.blocker_message().expect("unowned active observation");
        assert!(message.contains("last source observation"));
        assert!(message.contains("refresh/inspect Codex"));

        let mut run = control_run(9, 1);
        run.state = ControlRunState::Active;
        run.source_thread_id = Some("synthetic-thread".to_owned());
        app.runs = vec![run];
        assert!(app.blocker_message().is_none());

        app.runs[0].project_id = 2;
        assert!(
            app.blocker_message()
                .is_some_and(|message| message.contains("no matching Skein owner"))
        );

        app.runs[0].project_id = 1;
        app.runs[0].state = ControlRunState::RecoveryRequired;
        assert!(
            app.blocker_message()
                .is_some_and(|message| message.contains("read/reconcile"))
        );
    }

    #[test]
    fn compact_ui_renders_library_composer_and_help() {
        let mut app = empty_app();
        app.focus = Focus::Composer;
        app.composer = "synthetic prompt".to_owned();
        let backend = TestBackend::new(100, 28);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| app.render(frame)).expect("render");
        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("SESSION SKEIN"));
        assert!(rendered.contains("Global conductor composer"));
        assert!(rendered.contains("synthetic prompt"));
        assert!(rendered.contains("F2 arm"));
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        let mut app = empty_app();
        app.focus = Focus::Composer;
        app.composer = "synthetic".to_owned();
        let backend = TestBackend::new(1, 1);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| app.render(frame)).expect("render");
    }

    fn project_card(id: i64, name: &str) -> ProjectCard {
        ProjectCard {
            project: Project {
                id,
                name: name.to_owned(),
                path: PathBuf::from(format!("/synthetic/{name}")),
                updated_at: 1,
                metadata_refreshed_at: None,
                git: None,
            },
            title: name.to_owned(),
            narrative: format!("Synthetic {name} project."),
            facts: ProjectCardFacts {
                linked_sessions: 0,
                latest_session_at: None,
                control_runs: 0,
                active_runs: 0,
                completed_runs: 0,
                failed_runs: 0,
                interrupted_runs: 0,
                recovery_runs: 0,
                latest_control_at: None,
            },
            last_activity_at: None,
            generated: true,
            persisted: false,
        }
    }

    fn control_run(id: i64, project_id: i64) -> ControlRun {
        ControlRun {
            id,
            run_key: format!("synthetic-run-{id}"),
            project_id,
            project_name: "first".to_owned(),
            working_directory: PathBuf::from("/synthetic/first"),
            state: ControlRunState::Starting,
            ownership_mode: "worker".to_owned(),
            source_thread_id: None,
            source_session_id: None,
            created_at: 1,
            updated_at: 1,
            terminal_at: None,
            sandbox_mode: "danger_full_access".to_owned(),
            approval_mode: "never".to_owned(),
            network_access: true,
            full_access_acknowledged_at: 1,
        }
    }

    fn session_metadata(project_id: i64, ephemeral: bool, status: &str) -> SessionMetadata {
        SessionMetadata {
            id: 1,
            source_kind: "codex".to_owned(),
            source_thread_id: "synthetic-thread".to_owned(),
            project_id: Some(project_id),
            project_link_kind: skein_core::ProjectLinkKind::Automatic,
            source_cwd: PathBuf::from("/synthetic/first"),
            source_updated_at: 1,
            last_seen_at: 1,
            source_label: "cli".to_owned(),
            observed_status_label: status.to_owned(),
            ephemeral,
            text_imported: false,
        }
    }

    fn redacted_event(sequence: u64, kind: &str) -> RedactedWorkerEvent {
        RedactedWorkerEvent {
            sequence,
            kind: kind.to_owned(),
            thread_id: None,
            turn_id: None,
            status: None,
            item_type: None,
            delta_bytes: None,
        }
    }

    fn buffer_text(buffer: &Buffer) -> String {
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }
}
