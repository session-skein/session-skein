mod worker_runtime;

use std::io::Read;
use std::path::PathBuf;

use clap::ArgGroup;
use clap::Args;
use clap::Parser;
use clap::Subcommand;
use serde::Serialize;
use skein_codex::ControlClient;
use skein_codex::ControlEvent;
use skein_codex::DiscoveryOptions;
use skein_core::NewControlRun;
use skein_core::Registry;
use skein_core::SessionImportReport;
use skein_core::SessionObservation;
use skein_core::SkeinPaths;

const MAX_CONTROL_PROMPT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Parser)]
#[command(name = "skein", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show paths, state, and schema health without modifying anything.
    Doctor(OutputArgs),
    /// Initialize the private local state database.
    Init(OutputArgs),
    /// Manage explicitly registered projects.
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Preview data from external agent adapters without persisting it.
    Import {
        #[command(subcommand)]
        command: ImportCommand,
    },
    /// Synchronize and inspect the durable session catalog.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Run and inspect explicitly targeted Codex work.
    Control {
        #[command(subcommand)]
        command: ControlCommand,
    },
    /// Run reconnectable, explicitly targeted Codex jobs.
    Worker {
        #[command(subcommand)]
        command: WorkerCommand,
    },
}

#[derive(Debug, Subcommand)]
enum WorkerCommand {
    /// List reconnectable worker jobs without exposing IPC credentials.
    List {
        /// Show only workers with nonterminal process state.
        #[arg(long)]
        active: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Start a new reconnectable Codex job in a registered project.
    Start(WorkerStartArgs),
    /// Resume a cataloged, project-bound Codex thread in a reconnectable job.
    Resume(WorkerResumeArgs),
    /// Show durable redacted job and worker state.
    Status {
        /// Numeric Skein run identifier returned by start or resume.
        run_id: i64,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Attach to redacted live events without owning the worker lifetime.
    Watch {
        /// Numeric Skein run identifier returned by start or resume.
        run_id: i64,
        /// Emit newline-delimited JSON events.
        #[arg(long)]
        jsonl: bool,
    },
    /// Stop an idle or terminal worker; active runs are refused.
    Stop { run_id: i64 },
    /// Interrupt the exact active Codex turn owned by a worker.
    Interrupt { run_id: i64 },
    /// Append stdin text to the exact active turn owned by a worker.
    Steer {
        run_id: i64,
        /// Stable UUID for safely retrying a lost client response.
        #[arg(long)]
        request_id: Option<String>,
    },
    /// Read redacted authoritative source identity and status without resuming it.
    Read {
        run_id: i64,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Reconcile a lost worker run against its exact Codex source turn.
    Reconcile {
        run_id: i64,
        /// Stable UUID for safely retrying this read-only probe.
        #[arg(long)]
        request_id: Option<String>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Internal worker process entry point.
    #[command(hide = true)]
    Serve { run_id: i64 },
    /// Internal Codex child containment proxy.
    #[command(hide = true)]
    CodexGuard,
}

#[derive(Debug, Args)]
struct WorkerStartArgs {
    /// Existing registered project directory.
    project: PathBuf,
    /// Explicitly authorize danger-full-access with approval policy never.
    #[arg(long)]
    full_access: bool,
    /// Remain attached to redacted live events until terminal completion.
    #[arg(long)]
    follow: bool,
    /// Emit machine-readable JSON.
    #[arg(long, conflicts_with = "jsonl")]
    json: bool,
    /// Emit a machine-readable JSONL stream when following.
    #[arg(long, conflicts_with = "json")]
    jsonl: bool,
}

#[derive(Debug, Args)]
struct WorkerResumeArgs {
    /// Cataloged Codex thread bound to its selected project.
    thread_id: String,
    /// Existing registered project directory.
    project: PathBuf,
    /// Explicitly authorize danger-full-access with approval policy never.
    #[arg(long)]
    full_access: bool,
    /// Remain attached to redacted live events until terminal completion.
    #[arg(long)]
    follow: bool,
    /// Emit machine-readable JSON.
    #[arg(long, conflicts_with = "jsonl")]
    json: bool,
    /// Emit a machine-readable JSONL stream when following.
    #[arg(long, conflicts_with = "json")]
    jsonl: bool,
}

#[derive(Debug, Subcommand)]
enum ControlCommand {
    /// Start or resume one audited foreground Codex turn.
    Codex(ControlCodexArgs),
    /// List redaction-safe Skein-owned runs.
    List(OutputArgs),
    /// Show one run and its audit actions without prompt or output content.
    Show {
        /// Numeric Skein run identifier.
        run_id: i64,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Force-quarantine in-flight records after verifying their controller is dead.
    MarkStale {
        /// Acknowledge that this can quarantine a still-live foreground controller.
        #[arg(long)]
        force: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct ControlCodexArgs {
    /// Existing registered project directory.
    project: PathBuf,
    /// Resume this opaque Codex thread instead of starting a new one.
    #[arg(long)]
    resume: Option<String>,
    /// Explicitly authorize danger-full-access with approval policy never.
    #[arg(long)]
    full_access: bool,
    /// Display live agent-message deltas; content is never stored by Skein.
    #[arg(long)]
    include_content: bool,
    /// Emit newline-delimited JSON events and a final run record.
    #[arg(long)]
    jsonl: bool,
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    /// Synchronize a bounded page from a first-class agent runtime.
    Sync {
        #[command(subcommand)]
        command: SessionSyncCommand,
    },
    /// List durable sessions newest-first.
    List(SessionListArgs),
    /// Show one durable session by its source-owned thread ID.
    Show(SessionIdentityArgs),
    /// Bind one durable session explicitly to a registered project.
    Bind(SessionBindArgs),
    /// Leave one durable session explicitly unassigned.
    Unbind(SessionIdentityArgs),
}

#[derive(Debug, Subcommand)]
enum SessionSyncCommand {
    /// Synchronize one bounded Codex thread-list page.
    Codex(CodexSyncArgs),
}

#[derive(Debug, Args)]
struct CodexSyncArgs {
    /// Maximum threads in this page.
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=1000))]
    limit: u32,
    /// Opaque next-page cursor returned during the current scan.
    #[arg(long)]
    cursor: Option<String>,
    /// Store thread titles and first-message previews in private local state.
    #[arg(long)]
    include_text: bool,
    /// Allow Codex to scan JSONL rollouts to repair its state index.
    #[arg(long)]
    repair_source_index: bool,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("session_filter").args(["project", "unmatched"])))]
struct SessionListArgs {
    /// Show sessions linked to this registered project.
    #[arg(long)]
    project: Option<PathBuf>,
    /// Show sessions without a project link.
    #[arg(long)]
    unmatched: bool,
    /// Filter by adapter identity, such as `codex`.
    #[arg(long)]
    source: Option<String>,
    /// Include stored thread names and first-message previews in output.
    #[arg(long)]
    include_text: bool,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct SessionIdentityArgs {
    /// Opaque thread identifier owned by the source adapter.
    source_thread_id: String,
    /// Adapter identity.
    #[arg(long, default_value = "codex")]
    source: String,
    /// Include stored thread names and first-message previews in output.
    #[arg(long)]
    include_text: bool,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct SessionBindArgs {
    /// Opaque thread identifier owned by the source adapter.
    source_thread_id: String,
    /// Existing registered project directory.
    project: PathBuf,
    /// Adapter identity.
    #[arg(long, default_value = "codex")]
    source: String,
    /// Include stored thread names and first-message previews in output.
    #[arg(long)]
    include_text: bool,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum ImportCommand {
    /// Read Codex conversation metadata through the local app-server.
    Codex {
        #[command(subcommand)]
        command: CodexImportCommand,
    },
}

#[derive(Debug, Subcommand)]
enum CodexImportCommand {
    /// Preview one bounded page without writing Session Skein state.
    Preview(CodexPreviewArgs),
}

#[derive(Debug, Args)]
struct CodexPreviewArgs {
    /// Maximum threads in this page.
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=1000))]
    limit: u32,
    /// Opaque next-page cursor returned by a previous preview.
    #[arg(long)]
    cursor: Option<String>,
    /// Include thread titles and first-message previews in terminal output.
    #[arg(long)]
    include_text: bool,
    /// Allow Codex to scan JSONL rollouts to repair its state index.
    #[arg(long)]
    repair_source_index: bool,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct OutputArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum ProjectCommand {
    /// Add or update a project by its canonical path.
    Add {
        /// Existing project directory.
        path: PathBuf,
        /// Override the directory-derived display name.
        #[arg(long)]
        name: Option<String>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show one registered project and its latest metadata snapshot.
    Show {
        /// Existing registered project directory.
        path: PathBuf,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Incrementally refresh bounded metadata for one project or all projects.
    Refresh(RefreshArgs),
    /// List projects in stable display order.
    List(OutputArgs),
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("scope").required(true).args(["path", "all"])))]
struct RefreshArgs {
    /// Existing registered project directory.
    path: Option<PathBuf>,
    /// Refresh every explicitly registered project, sequentially.
    #[arg(long)]
    all: bool,
    /// Check tracked working-tree changes; untracked files remain excluded.
    #[arg(long)]
    working_tree: bool,
    /// Ignore the stored fingerprint and inspect metadata again.
    #[arg(long)]
    force: bool,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Serialize)]
struct DoctorReport {
    version: &'static str,
    config_dir: PathBuf,
    data_dir: PathBuf,
    database: PathBuf,
    database_exists: bool,
    schema_version: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexSyncReport {
    #[serde(flatten)]
    import: SessionImportReport,
    next_cursor: Option<String>,
    repaired_source_index: bool,
    text_imported: bool,
}

#[derive(Serialize)]
struct SessionView {
    #[serde(flatten)]
    session: skein_core::Session,
    text_redacted: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let paths = SkeinPaths::discover()?;

    match cli.command {
        Command::Doctor(output) => doctor(&paths, output.json)?,
        Command::Init(output) => {
            let registry = Registry::open(&paths)?;
            let report = report(&paths, Some(registry.schema_version()?));
            print_value(&report, output.json)?;
        }
        Command::Project { command } => {
            let registry = Registry::open(&paths)?;
            match command {
                ProjectCommand::Add { path, name, json } => {
                    let project = registry.add_project(&path, name.as_deref())?;
                    print_value(&project, json)?;
                }
                ProjectCommand::Show { path, json } => {
                    let project = registry.get_project(&path)?;
                    print_value(&project, json)?;
                }
                ProjectCommand::Refresh(args) => {
                    if args.all {
                        let reports = registry.refresh_all(args.working_tree, args.force)?;
                        print_value(&reports, args.json)?;
                    } else if let Some(path) = args.path {
                        let report =
                            registry.refresh_project(&path, args.working_tree, args.force)?;
                        print_value(&report, args.json)?;
                    }
                }
                ProjectCommand::List(output) => {
                    let projects = registry.list_projects()?;
                    print_value(&projects, output.json)?;
                }
            }
        }
        Command::Import { command } => match command {
            ImportCommand::Codex { command } => match command {
                CodexImportCommand::Preview(args) => {
                    let page = skein_codex::discover(&DiscoveryOptions {
                        limit: args.limit,
                        cursor: args.cursor,
                        use_state_db_only: !args.repair_source_index,
                        include_text: args.include_text,
                    })?;
                    print_value(&page, args.json)?;
                }
            },
        },
        Command::Session { command } => match command {
            SessionCommand::Sync { command } => match command {
                SessionSyncCommand::Codex(args) => sync_codex(&paths, args)?,
            },
            SessionCommand::List(args) => {
                let registry = Registry::open(&paths)?;
                let project_id = args
                    .project
                    .as_deref()
                    .map(|path| registry.get_project(path).map(|project| project.id))
                    .transpose()?;
                let sessions = registry
                    .list_sessions()?
                    .into_iter()
                    .filter(|session| {
                        project_id.is_none_or(|id| session.project_id == Some(id))
                            && (!args.unmatched || session.project_id.is_none())
                            && args
                                .source
                                .as_deref()
                                .is_none_or(|source| session.source_kind == source)
                    })
                    .map(|session| session_view(session, args.include_text))
                    .collect::<Vec<_>>();
                print_value(&sessions, args.json)?;
            }
            SessionCommand::Show(args) => {
                let registry = Registry::open(&paths)?;
                let session = registry
                    .session_by_source(&args.source, &args.source_thread_id)?
                    .ok_or_else(|| skein_core::Error::SessionNotFound {
                        source_kind: args.source,
                        source_thread_id: args.source_thread_id,
                    })?;
                print_value(&session_view(session, args.include_text), args.json)?;
            }
            SessionCommand::Bind(args) => {
                let registry = Registry::open(&paths)?;
                let session =
                    registry.bind_session(&args.source, &args.source_thread_id, &args.project)?;
                print_value(&session_view(session, args.include_text), args.json)?;
            }
            SessionCommand::Unbind(args) => {
                let registry = Registry::open(&paths)?;
                let session = registry.unbind_session(&args.source, &args.source_thread_id)?;
                print_value(&session_view(session, args.include_text), args.json)?;
            }
        },
        Command::Control { command } => match command {
            ControlCommand::Codex(args) => control_codex(&paths, args)?,
            ControlCommand::List(output) => {
                let registry = Registry::open(&paths)?;
                print_value(&registry.list_control_runs()?, output.json)?;
            }
            ControlCommand::Show { run_id, json } => {
                let registry = Registry::open(&paths)?;
                print_value(&registry.control_run_detail(run_id)?, json)?;
            }
            ControlCommand::MarkStale { force, json } => {
                let mut registry = Registry::open(&paths)?;
                print_value(&registry.mark_stale_control_runs(force)?, json)?;
            }
        },
        Command::Worker { command } => match command {
            WorkerCommand::List { active, json } => {
                print_value(&worker_runtime::durable_list(&paths, active)?, json)?;
            }
            WorkerCommand::Start(args) => worker_start(&paths, args)?,
            WorkerCommand::Resume(args) => worker_resume(&paths, args)?,
            WorkerCommand::Status { run_id, json } => {
                print_value(&worker_runtime::durable_snapshot(&paths, run_id)?, json)?;
            }
            WorkerCommand::Watch { run_id, jsonl } => {
                let run = worker_runtime::watch(&paths, run_id, jsonl)?;
                if jsonl {
                    println!(
                        "{}",
                        serde_json::to_string(&serde_json::json!({
                            "type": "worker_run",
                            "run": run
                        }))?
                    );
                } else {
                    println!("\nrun {}: {:?}", run.id, run.state);
                }
            }
            WorkerCommand::Stop { run_id } => {
                worker_runtime::stop(&paths, run_id)?;
                println!("stopped worker for run {run_id}");
            }
            WorkerCommand::Interrupt { run_id } => {
                worker_runtime::interrupt(&paths, run_id)?;
                println!("interrupt accepted for run {run_id}");
            }
            WorkerCommand::Steer { run_id, request_id } => {
                let prompt = read_control_prompt()?;
                let request_id = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let (action_id, queued) =
                    worker_runtime::steer(&paths, run_id, &request_id, prompt)?;
                if queued {
                    println!(
                        "steer queued for run {run_id} as action {action_id} (request {request_id})"
                    );
                } else {
                    println!(
                        "steer request reused action {action_id} for run {run_id} (request {request_id})"
                    );
                }
            }
            WorkerCommand::Read { run_id, json } => {
                print_value(&worker_runtime::read_source(&paths, run_id)?, json)?;
            }
            WorkerCommand::Reconcile {
                run_id,
                request_id,
                json,
            } => {
                let request_id = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                print_value(
                    &worker_runtime::reconcile(&paths, run_id, &request_id)?,
                    json,
                )?;
            }
            WorkerCommand::Serve { run_id } => worker_runtime::serve(paths, run_id)?,
            WorkerCommand::CodexGuard => worker_runtime::codex_guard()?,
        },
    }

    Ok(())
}

fn worker_start(
    paths: &SkeinPaths,
    args: WorkerStartArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    worker_launch(
        paths,
        &args.project,
        None,
        args.full_access,
        args.follow,
        args.json,
        args.jsonl,
    )
}

fn worker_resume(
    paths: &SkeinPaths,
    args: WorkerResumeArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    worker_launch(
        paths,
        &args.project,
        Some(&args.thread_id),
        args.full_access,
        args.follow,
        args.json,
        args.jsonl,
    )
}

fn worker_launch(
    paths: &SkeinPaths,
    project: &std::path::Path,
    resume_thread_id: Option<&str>,
    full_access: bool,
    follow: bool,
    json: bool,
    jsonl: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !full_access {
        return Err(skein_core::Error::InvalidControlRequest(
            "pass --full-access to acknowledge danger-full-access with approvals disabled"
                .to_owned(),
        )
        .into());
    }
    if json && follow {
        return Err(skein_core::Error::InvalidControlRequest(
            "use --jsonl, not --json, when --follow is enabled".to_owned(),
        )
        .into());
    }
    let prompt = read_control_prompt()?;
    // Validate ChatGPT authentication before creating durable run or worker state.
    drop(ControlClient::connect()?);
    let mut registry = Registry::open(paths)?;
    let plan = registry.plan_control_run(&NewControlRun {
        project_path: project,
        resume_thread_id,
        prompt: &prompt,
        full_access_acknowledged: true,
    })?;
    let snapshot = worker_runtime::launch_worker(paths, &mut registry, plan.run_id, prompt)?;
    if jsonl {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "type": "worker_snapshot",
                "snapshot": snapshot
            }))?
        );
    } else if json {
        print_value(&snapshot, true)?;
    } else {
        println!("started reconnectable run {}", plan.run_id);
    }
    if follow {
        let run = worker_runtime::watch(paths, plan.run_id, jsonl)?;
        if jsonl {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({"type":"worker_run","run":run}))?
            );
        } else {
            println!("\nrun {}: {:?}", run.id, run.state);
        }
    }
    Ok(())
}

fn read_control_prompt() -> Result<String, Box<dyn std::error::Error>> {
    let mut prompt = String::new();
    std::io::stdin()
        .take((MAX_CONTROL_PROMPT_BYTES + 1) as u64)
        .read_to_string(&mut prompt)?;
    if prompt.len() > MAX_CONTROL_PROMPT_BYTES {
        return Err(skein_core::Error::InvalidControlRequest(format!(
            "prompt exceeds the {MAX_CONTROL_PROMPT_BYTES}-byte control limit"
        ))
        .into());
    }
    if prompt.trim().is_empty() {
        return Err(skein_core::Error::InvalidControlRequest(
            "provide the prompt on standard input".to_owned(),
        )
        .into());
    }
    Ok(prompt)
}

fn control_codex(
    paths: &SkeinPaths,
    args: ControlCodexArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    if !args.full_access {
        return Err(skein_core::Error::InvalidControlRequest(
            "pass --full-access to acknowledge danger-full-access with approvals disabled"
                .to_owned(),
        )
        .into());
    }
    let prompt = read_control_prompt()?;

    let mut client = ControlClient::connect()?;
    let mut registry = Registry::open(paths)?;
    let plan = registry.plan_control_run(&NewControlRun {
        project_path: &args.project,
        resume_thread_id: args.resume.as_deref(),
        prompt: &prompt,
        full_access_acknowledged: true,
    })?;

    registry.begin_control_action(plan.thread_action_id)?;
    let thread = match args.resume.as_deref() {
        Some(thread_id) => client.resume_thread(thread_id, &plan.working_directory),
        None => client.start_thread(&plan.working_directory),
    };
    let thread = match thread {
        Ok(thread) => thread,
        Err(error) => {
            registry.mark_control_uncertain(plan.run_id)?;
            return Err(error.into());
        }
    };
    if let Err(error) = registry.acknowledge_thread_action(
        plan.thread_action_id,
        &thread.thread_id,
        Some(&thread.session_id),
    ) {
        registry.mark_control_uncertain(plan.run_id)?;
        return Err(error.into());
    }

    registry.begin_control_action(plan.turn_action_id)?;
    let turn = match client.start_turn(
        &thread.thread_id,
        &prompt,
        &plan.client_message_id,
        &plan.working_directory,
    ) {
        Ok(turn) => turn,
        Err(error) => {
            registry.mark_control_uncertain(plan.run_id)?;
            return Err(error.into());
        }
    };
    if let Err(error) = registry.acknowledge_turn_action(plan.turn_action_id, &turn.turn_id) {
        registry.mark_control_uncertain(plan.run_id)?;
        return Err(error.into());
    }

    loop {
        let event = match client.next_event() {
            Ok(event) => event,
            Err(error) => {
                registry.mark_control_uncertain(plan.run_id)?;
                return Err(error.into());
            }
        };
        if control_event_matches(&event, &thread.thread_id, &turn.turn_id) {
            emit_control_event(&event, args.include_content, args.jsonl)?;
        }
        if let ControlEvent::TurnCompleted {
            thread_id,
            turn_id,
            status,
        } = &event
            && thread_id == &thread.thread_id
            && turn_id == &turn.turn_id
        {
            let run = registry.complete_control_run(plan.run_id, status)?;
            if args.jsonl {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "type": "control_run",
                        "run": run,
                        "contentRedacted": !args.include_content
                    }))?
                );
            } else {
                println!("\nrun {}: {:?}", run.id, run.state);
            }
            break;
        }
    }
    Ok(())
}

fn control_event_matches(event: &ControlEvent, thread_id: &str, turn_id: &str) -> bool {
    match event {
        ControlEvent::TurnStarted {
            thread_id: event_thread,
            turn_id: event_turn,
        }
        | ControlEvent::AgentMessageDelta {
            thread_id: event_thread,
            turn_id: event_turn,
            ..
        }
        | ControlEvent::ItemStarted {
            thread_id: event_thread,
            turn_id: event_turn,
            ..
        }
        | ControlEvent::ItemCompleted {
            thread_id: event_thread,
            turn_id: event_turn,
            ..
        }
        | ControlEvent::RetryingError {
            thread_id: event_thread,
            turn_id: event_turn,
            ..
        }
        | ControlEvent::TurnCompleted {
            thread_id: event_thread,
            turn_id: event_turn,
            ..
        } => event_thread == thread_id && event_turn == turn_id,
        ControlEvent::ThreadStatusChanged {
            thread_id: event_thread,
            ..
        } => event_thread == thread_id,
        ControlEvent::Unknown { .. } => true,
    }
}

fn emit_control_event(
    event: &ControlEvent,
    include_content: bool,
    jsonl: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if jsonl {
        let value = match event {
            ControlEvent::AgentMessageDelta {
                thread_id,
                turn_id,
                delta,
            } if !include_content => serde_json::json!({
                "type": "agent_message_delta",
                "threadId": thread_id,
                "turnId": turn_id,
                "deltaBytes": delta.len(),
                "contentRedacted": true
            }),
            _ => serde_json::to_value(event)?,
        };
        println!("{}", serde_json::to_string(&value)?);
    } else {
        match event {
            ControlEvent::AgentMessageDelta { delta, .. } if include_content => print!("{delta}"),
            ControlEvent::TurnStarted { .. } => eprintln!("Codex turn started"),
            ControlEvent::TurnCompleted { status, .. } => {
                eprintln!("Codex turn completed: {status}")
            }
            _ => {}
        }
    }
    Ok(())
}

fn sync_codex(paths: &SkeinPaths, args: CodexSyncArgs) -> Result<(), Box<dyn std::error::Error>> {
    let page = skein_codex::discover(&DiscoveryOptions {
        limit: args.limit,
        cursor: args.cursor,
        use_state_db_only: !args.repair_source_index,
        include_text: args.include_text,
    })?;
    if args.include_text && !args.json {
        eprintln!(
            "warning: storing Codex thread names and first-message previews in private local state"
        );
    }
    let observations = page
        .threads
        .into_iter()
        .map(|thread| SessionObservation {
            source_kind: "codex".to_owned(),
            source_thread_id: thread.id,
            source_session_id: Some(thread.session_id),
            source_cwd: PathBuf::from(thread.cwd),
            source_created_at: thread.created_at,
            source_updated_at: thread.updated_at,
            source_label: thread.source,
            observed_status_label: thread.status,
            model_provider: Some(thread.model_provider),
            source_version: Some(thread.cli_version),
            parent_source_thread_id: thread.parent_thread_id,
            forked_from_source_thread_id: thread.forked_from_id,
            ephemeral: thread.ephemeral,
            name: thread.name,
            preview: thread.preview,
            text_imported: !thread.text_redacted,
        })
        .collect::<Vec<_>>();
    let mut registry = Registry::open(paths)?;
    let import = registry.import_sessions(&observations)?;
    print_value(
        &CodexSyncReport {
            import,
            next_cursor: page.next_cursor,
            repaired_source_index: page.repaired_source_index,
            text_imported: args.include_text,
        },
        args.json,
    )
}

fn session_view(mut session: skein_core::Session, include_text: bool) -> SessionView {
    let text_redacted = session.text_imported && !include_text;
    if !include_text {
        session.name = None;
        session.preview = None;
    }
    SessionView {
        session,
        text_redacted,
    }
}

fn doctor(paths: &SkeinPaths, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let database_exists = paths.database().is_file();
    let schema_version = if database_exists {
        Some(Registry::open_read_only(paths)?.schema_version()?)
    } else {
        None
    };
    print_value(&report(paths, schema_version), json)
}

fn report(paths: &SkeinPaths, schema_version: Option<i64>) -> DoctorReport {
    DoctorReport {
        version: env!("CARGO_PKG_VERSION"),
        config_dir: paths.config_dir.clone(),
        data_dir: paths.data_dir.clone(),
        database: paths.database(),
        database_exists: paths.database().is_file(),
        schema_version,
    }
}

fn print_value<T: Serialize>(value: &T, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{}", serde_json::to_string(value)?);
    }
    Ok(())
}
