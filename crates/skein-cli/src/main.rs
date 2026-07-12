mod indexing;
mod mcp;
mod output;
mod progress;
mod tui;
mod update;
mod worker_runtime;

use std::io::Read;
use std::path::PathBuf;

use chrono::Local;
use chrono::NaiveDate;
use chrono::TimeZone;
use clap::ArgGroup;
use clap::Args;
use clap::Parser;
use clap::Subcommand;
use serde::Serialize;
use skein_codex::ControlClient;
use skein_codex::ControlEvent;
use skein_codex::DiscoveryBounds;
use skein_codex::DiscoveryOptions;
use skein_core::NewControlRun;
use skein_core::Registry;
use skein_core::ScanRootOptions;
use skein_core::SessionImportReport;
use skein_core::SessionObservation;
use skein_core::SkeinPaths;
use uuid::Uuid;

const MAX_CONTROL_PROMPT_BYTES: usize = 1024 * 1024;
const MAX_MATCH_QUERY_BYTES: usize = 64 * 1024;

#[derive(Debug, Parser)]
#[command(name = "skein", version, about)]
struct Cli {
    /// Select human-readable or machine-readable command output.
    #[arg(long, global = true, value_enum, default_value_t = output::OutputFormat::Human)]
    format: output::OutputFormat,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show paths, state, and schema health without modifying anything.
    Doctor(OutputArgs),
    /// Initialize the private local state database.
    Init(OutputArgs),
    /// Show read-only freshness of durable Git, document, session, and context observations.
    Freshness {
        /// Label observations older than this many hours as stale.
        #[arg(long, default_value_t = 24, value_parser = clap::value_parser!(u32).range(0..=87_600))]
        stale_after_hours: u32,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Check for or install a verified release-owned update.
    Update {
        /// Exact target version; defaults to the approved preview channel.
        version: Option<String>,
        /// Verify and report without changing the installation.
        #[arg(long)]
        check: bool,
        /// Reinstall an unchanged version intentionally.
        #[arg(long)]
        force: bool,
        /// Permit an explicit downgrade after reviewing schema compatibility.
        #[arg(long)]
        allow_downgrade: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Manage explicitly registered projects.
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Manage user-approved roots for optional repository discovery.
    ScanRoot {
        #[command(subcommand)]
        command: ScanRootCommand,
    },
    /// Discover repositories and refresh Git, project-document, and enabled context indexes.
    Index(IndexArgs),
    /// Search project metadata, identity documents, and opted-in bounded context.
    Search {
        /// Search terms.
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        /// Maximum projects and document hits.
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..=50))]
        limit: u32,
        /// Permit explicitly imported session names/previews in metadata scoring.
        #[arg(long)]
        include_session_text: bool,
        /// Consult enabled private memory/session context after explicit user intent.
        #[arg(long)]
        deep_context: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Configure and search optional local Codex memory/session recall.
    Context {
        #[command(subcommand)]
        command: ContextCommand,
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
    /// Route one private prompt and dispatch only a unique high-confidence Codex worker.
    Conduct(ConductArgs),
    /// Open the standalone keyboard-first project/session conductor interface.
    Tui,
    /// Serve Session Skein tools to Codex over MCP stdio.
    Mcp(McpArgs),
    /// Rank registered projects and linked sessions without dispatching work.
    Match {
        /// Allow explicitly imported session names/previews to influence local scoring.
        #[arg(long)]
        include_text: bool,
        /// Maximum number of ranked projects to return.
        #[arg(long, default_value_t = 5)]
        limit: usize,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Render deterministic project cards and bounded activity digests.
    Summary {
        #[command(subcommand)]
        command: SummaryCommand,
    },
}

#[derive(Debug, Args)]
struct ConductArgs {
    /// Explicitly authorize danger-full-access with approval policy never.
    #[arg(long)]
    full_access: bool,
    /// Permit previously opted-in session names/previews to influence local routing.
    #[arg(long)]
    include_session_text: bool,
    /// Select one ranked project by stable ID when automatic routing is ambiguous.
    #[arg(long)]
    project_id: Option<i64>,
    /// Resume this ranked session exactly; requires --project-id.
    #[arg(long, requires = "project_id")]
    session_id: Option<String>,
    /// Stable UUID; reuse is a status lookup and never resubmits prompt content.
    #[arg(long)]
    request_id: Option<String>,
    /// Remain attached to redacted worker events until terminal completion.
    #[arg(long, conflicts_with = "json")]
    follow: bool,
    /// Emit one machine-readable launch object.
    #[arg(long, conflicts_with = "jsonl")]
    json: bool,
    /// Emit route, worker, redacted events, and terminal state as JSONL; implies follow.
    #[arg(long, conflicts_with = "json")]
    jsonl: bool,
}

#[derive(Debug, Args)]
struct McpArgs {
    /// Expose tools that can start, steer, interrupt, or reconcile Codex workers.
    #[arg(long)]
    allow_control: bool,
}

#[derive(Debug, Subcommand)]
enum SummaryCommand {
    /// Describe every registered project in stable order.
    Projects {
        #[arg(long)]
        json: bool,
    },
    /// Describe one registered project from already observed metadata.
    Project {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Summarize bounded metadata activity for one local calendar day.
    Day {
        /// Local date in YYYY-MM-DD form; defaults to today.
        date: Option<String>,
        #[arg(long)]
        json: bool,
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
    /// Observe durable redacted progress from a stable event cursor.
    Observe {
        run_id: i64,
        #[arg(long, default_value_t = 0)]
        after_cursor: i64,
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=100))]
        limit: u32,
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=30_000))]
        timeout_ms: u64,
        #[arg(long, conflicts_with = "jsonl")]
        json: bool,
        #[arg(long, conflicts_with = "json")]
        jsonl: bool,
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
    /// Synchronize a bounded Codex thread catalog, optionally following all pages.
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
    /// Follow Codex cursors until complete or the configured bounds are reached.
    #[arg(long)]
    all_pages: bool,
    /// Maximum pages when --all-pages is enabled.
    #[arg(long, default_value_t = 100, requires = "all_pages", value_parser = clap::value_parser!(u32).range(1..=100))]
    max_pages: u32,
    /// Maximum total threads when --all-pages is enabled.
    #[arg(long, default_value_t = 10_000, requires = "all_pages", value_parser = clap::value_parser!(u32).range(1..=10_000))]
    max_threads: u32,
    /// Store thread titles and first-message previews in private local state.
    #[arg(long)]
    include_text: bool,
    /// Allow Codex to scan JSONL rollouts to repair its state index.
    #[arg(long)]
    repair_source_index: bool,
    /// Import only source threads updated within this many days.
    #[arg(long, value_parser = clap::value_parser!(u16).range(0..=3650))]
    since_days: Option<u16>,
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

#[derive(Debug, Subcommand)]
enum ScanRootCommand {
    /// Add a root and immediately discover repositories beneath it.
    Add {
        /// Existing directory approved for discovery.
        path: PathBuf,
        /// Recursively discover nested Git repositories.
        #[arg(long)]
        recursive: bool,
        /// Maximum recursive directory depth (0 through 64; default 16).
        #[arg(long, requires = "recursive")]
        max_depth: Option<u16>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// List configured discovery roots and their policies.
    List(OutputArgs),
    /// Remove a discovery root without deleting its discovered projects.
    Remove {
        /// Stored root path; the directory need not still be mounted.
        path: PathBuf,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct IndexArgs {
    /// Refresh exactly one registered project; no discovery is performed.
    #[arg(long, value_name = "PATH", conflicts_with = "scan_root")]
    project: Option<PathBuf>,
    /// Discover and refresh exactly one configured scan root.
    #[arg(long, value_name = "PATH", conflicts_with = "project")]
    scan_root: Option<PathBuf>,
    /// Check tracked working-tree changes after discovery.
    #[arg(long)]
    working_tree: bool,
    /// Ignore stored Git fingerprints after discovery.
    #[arg(long)]
    force: bool,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum ContextToggle {
    Enable,
    Disable,
}

#[derive(Debug, Subcommand)]
enum ContextCommand {
    /// Show explicit memory/session recall settings.
    Status(OutputArgs),
    /// Enable or disable generated Codex memory-summary recall.
    Memories {
        state: ContextToggle,
        #[arg(long)]
        json: bool,
    },
    /// Enable or disable raw Codex session-message recall beneath approved roots.
    Sessions {
        state: ContextToggle,
        #[arg(long)]
        json: bool,
    },
    /// Atomically rebuild enabled context sources.
    Refresh {
        /// Override CODEX_HOME for this refresh.
        #[arg(long)]
        codex_home: Option<PathBuf>,
        /// Maximum source files considered across enabled sources.
        #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u32).range(1..=10_000))]
        max_files: u32,
        #[arg(long)]
        json: bool,
    },
    /// Search opted-in private context documents with bounded snippets.
    Search {
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..=100))]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
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
    since_days: Option<u16>,
    window_start_at: Option<i64>,
    source_threads_selected: usize,
    page_count: u32,
    complete: bool,
    observed_at: i64,
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
    output::set_format(cli.format);
    let paths = SkeinPaths::discover()?;

    match cli.command {
        Command::Doctor(output) => doctor(&paths, output.json)?,
        Command::Init(output) => {
            let registry = Registry::open(&paths)?;
            let report = report(&paths, Some(registry.schema_version()?));
            print_value(&report, output.json)?;
        }
        Command::Freshness {
            stale_after_hours,
            json,
        } => {
            let registry = Registry::open_read_only(&paths)?;
            let stale_after = i64::from(stale_after_hours).saturating_mul(60 * 60);
            print_value(
                &registry.catalog_freshness(unix_timestamp(), stale_after)?,
                json,
            )?;
        }
        Command::Update {
            version,
            check,
            force,
            allow_downgrade,
            json,
        } => {
            update::run(update::UpdateOptions {
                version,
                check,
                force,
                allow_downgrade,
                json: json || output::is_json(),
            })?;
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
                        let reports =
                            refresh_git_resilient(&registry, args.working_tree, args.force)?;
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
        Command::ScanRoot { command } => {
            let mut registry = Registry::open(&paths)?;
            match command {
                ScanRootCommand::Add {
                    path,
                    recursive,
                    max_depth,
                    json,
                } => {
                    let root = registry.add_scan_root(
                        &path,
                        ScanRootOptions {
                            recursive,
                            max_depth,
                        },
                    )?;
                    let discovery = registry.discover_scan_root(&root.path)?;
                    print_value(
                        &serde_json::json!({
                            "root": root,
                            "discovery": discovery
                        }),
                        json,
                    )?;
                }
                ScanRootCommand::List(output) => {
                    print_value(&registry.list_scan_roots()?, output.json)?;
                }
                ScanRootCommand::Remove { path, json } => {
                    print_value(&registry.remove_scan_root(&path)?, json)?;
                }
            }
        }
        Command::Index(args) => {
            let scope = match (args.project, args.scan_root) {
                (Some(path), None) => indexing::IndexScope::Project(path),
                (None, Some(path)) => indexing::IndexScope::ScanRoot(path),
                (None, None) => indexing::IndexScope::All,
                (Some(_), Some(_)) => unreachable!("clap rejects mutually exclusive selectors"),
            };
            let progress = progress::Progress::cli(args.json);
            let report = indexing::refresh_index_with_progress(
                &paths,
                scope,
                args.working_tree,
                args.force,
                |stage| progress.stage(stage),
            )?;
            print_value(&report, args.json)?;
        }
        Command::Search {
            query,
            limit,
            include_session_text,
            deep_context,
            json,
        } => {
            let query = query.join(" ");
            let limit = usize::try_from(limit)?;
            let registry = Registry::open_read_only(&paths)?;
            let metadata = registry.match_metadata(&skein_core::MatchOptions {
                query: &query,
                include_text: include_session_text,
                limit,
                now: unix_timestamp(),
            })?;
            let documents = registry.search_project_documents(&query, limit)?;
            let settings = registry.get_recall_settings()?;
            let freshness = registry
                .catalog_freshness(unix_timestamp(), skein_core::DEFAULT_STALE_AFTER_SECONDS)?;
            let mut sources_consulted = vec!["project_metadata", "project_documents"];
            if deep_context && settings.include_codex_memories {
                sources_consulted.push("codex_memory");
            }
            if deep_context && settings.include_codex_sessions {
                sources_consulted.push("codex_session");
            }
            let context = if deep_context {
                registry.search_context_documents(&query, limit)?
            } else {
                Vec::new()
            };
            let context_truncated = context.len() == limit;
            print_value(
                &serde_json::json!({
                    "metadata": metadata,
                    "documents": documents,
                    "context": context,
                    "recall": {
                        "mode": if deep_context { "deep_private" } else { "quick_indexed" },
                        "sourcesConsulted": sources_consulted,
                        "privateContextAuthorized": settings.include_codex_memories || settings.include_codex_sessions,
                        "privateSources": settings,
                        "contextFreshness": freshness.context,
                        "limit": limit,
                        "contextReturned": context.len(),
                        "contextPossiblyTruncated": context_truncated,
                        "escalationSuggested": !deep_context && (settings.include_codex_memories || settings.include_codex_sessions),
                        "escalationReason": (!deep_context && (settings.include_codex_memories || settings.include_codex_sessions)).then_some("quick recall did not consult enabled private context"),
                        "nextCommand": (!deep_context).then_some("skein search --deep-context QUERY")
                    }
                }),
                json,
            )?;
        }
        Command::Context { command } => match command {
            ContextCommand::Status(output) => {
                let registry = Registry::open(&paths)?;
                print_value(&registry.get_recall_settings()?, output.json)?;
            }
            ContextCommand::Memories { state, json } => {
                let registry = Registry::open(&paths)?;
                let mut settings = registry.get_recall_settings()?;
                settings.include_codex_memories = matches!(state, ContextToggle::Enable);
                print_value(&registry.set_recall_settings(settings)?, json)?;
            }
            ContextCommand::Sessions { state, json } => {
                let registry = Registry::open(&paths)?;
                let mut settings = registry.get_recall_settings()?;
                settings.include_codex_sessions = matches!(state, ContextToggle::Enable);
                print_value(&registry.set_recall_settings(settings)?, json)?;
            }
            ContextCommand::Refresh {
                codex_home: override_home,
                max_files,
                json,
            } => {
                let progress = progress::Progress::cli(json);
                progress.stage("scanning enabled context sources");
                let mut registry = Registry::open(&paths)?;
                let report = registry.refresh_context_documents(
                    &codex_home(override_home)?,
                    skein_core::ContextDocumentRefreshOptions {
                        max_files: usize::try_from(max_files)?,
                    },
                )?;
                progress.stage("context refresh complete");
                print_value(&report, json)?;
            }
            ContextCommand::Search { query, limit, json } => {
                let registry = Registry::open_read_only(&paths)?;
                print_value(
                    &registry
                        .search_context_documents(&query.join(" "), usize::try_from(limit)?)?,
                    json,
                )?;
            }
        },
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
            WorkerCommand::Observe {
                run_id,
                after_cursor,
                limit,
                timeout_ms,
                json,
                jsonl,
            } => {
                let report = worker_runtime::observe(
                    &paths,
                    run_id,
                    after_cursor,
                    usize::try_from(limit)?,
                    std::time::Duration::from_millis(timeout_ms),
                )?;
                if jsonl {
                    for event in &report.observation.events {
                        println!("{}", serde_json::to_string(event)?);
                    }
                    println!(
                        "{}",
                        serde_json::to_string(&serde_json::json!({
                            "type": "observation_checkpoint",
                            "report": report
                        }))?
                    );
                } else {
                    print_value(&report, json)?;
                }
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
                let report = worker_runtime::interrupt(&paths, run_id)?;
                print_value(&report, false)?;
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
        Command::Conduct(args) => conduct(&paths, args)?,
        Command::Tui => tui::run(paths)?,
        Command::Mcp(args) => mcp::run(paths, args.allow_control)?,
        Command::Match {
            include_text,
            limit,
            json,
        } => {
            if !(1..=50).contains(&limit) {
                return Err(skein_core::Error::InvalidControlRequest(
                    "match limit must be in 1..=50".to_owned(),
                )
                .into());
            }
            let query = read_match_query()?;
            let registry = Registry::open(&paths)?;
            let report = registry.match_metadata(&skein_core::MatchOptions {
                query: &query,
                include_text,
                limit,
                now: unix_timestamp(),
            })?;
            print_match_report(&report, json)?;
        }
        Command::Summary { command } => {
            let registry = Registry::open(&paths)?;
            match command {
                SummaryCommand::Projects { json } => {
                    let cards = registry.project_cards()?;
                    if json || output::is_json() {
                        print_value(&cards, true)?;
                    } else {
                        for card in cards {
                            println!("{}", card.narrative);
                        }
                    }
                }
                SummaryCommand::Project { path, json } => {
                    let card = registry.project_card(&path)?;
                    if json || output::is_json() {
                        print_value(&card, true)?;
                    } else {
                        println!("{}", card.narrative);
                    }
                }
                SummaryCommand::Day { date, json } => {
                    let (date, timezone, start_at, end_at) = local_day_bounds(date.as_deref())?;
                    let summary = registry.day_summary(&date, &timezone, start_at, end_at)?;
                    if json || output::is_json() {
                        print_value(&summary, true)?;
                    } else {
                        println!("{}", summary.narrative);
                        for project in &summary.projects {
                            println!(
                                "- {}: {} session metadata observation(s), {} run(s) created",
                                project.project_name,
                                project.session_observations,
                                project.runs_created
                            );
                        }
                    }
                }
            }
        }
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

fn conduct(paths: &SkeinPaths, args: ConductArgs) -> Result<(), Box<dyn std::error::Error>> {
    if !args.full_access {
        return Err(skein_core::Error::InvalidControlRequest(
            "pass --full-access to acknowledge danger-full-access with approvals disabled"
                .to_owned(),
        )
        .into());
    }
    let prompt = read_match_query()?;
    let request_id = args
        .request_id
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    if Uuid::parse_str(&request_id).is_err() {
        return Err(skein_core::Error::InvalidControlRequest(
            "conductor request id must be a UUID".to_owned(),
        )
        .into());
    }

    let read_only = Registry::open_read_only(paths)?;
    if read_only.schema_version()? >= 7
        && let Some(_decision) = read_only.conductor_decision_by_request_id(&request_id)?
    {
        drop(read_only);
        let mut registry = Registry::open(paths)?;
        worker_runtime::recover_expired(&mut registry, paths)?;
        let decision = registry
            .conductor_decision_by_request_id(&request_id)?
            .ok_or("conductor decision disappeared during recovery")?;
        let run = registry
            .control_run(decision.run_id)?
            .ok_or("conductor run disappeared")?;
        let worker = registry.control_worker(decision.run_id)?;
        print_conductor_status(
            &decision,
            &run,
            worker.as_ref(),
            true,
            args.json,
            args.jsonl,
        )?;
        if args.follow || args.jsonl {
            let terminal = worker_runtime::watch(paths, decision.run_id, args.jsonl)?;
            print_conductor_terminal(&terminal, args.jsonl);
        }
        return Ok(());
    }

    let explicit_selection = args.project_id.is_some();
    let preflight = read_only.match_conductor_metadata(&skein_core::MatchOptions {
        query: &prompt,
        include_text: args.include_session_text,
        limit: if explicit_selection {
            skein_core::MAX_MATCH_CANDIDATES
        } else {
            5
        },
        now: unix_timestamp(),
    })?;
    let Some(recommendation) = preflight.recommendation.as_ref() else {
        print_route_refusal(&preflight, args.json, args.jsonl)?;
        return Err(
            skein_core::Error::InvalidControlRequest("route_refused:no_match".to_owned()).into(),
        );
    };
    if !explicit_selection
        && (recommendation.confidence != skein_core::MatchConfidence::High
            || recommendation.ambiguous
            || !recommendation.dispatchable)
    {
        print_route_refusal(&preflight, args.json, args.jsonl)?;
        return Err(skein_core::Error::InvalidControlRequest(
            "route_refused:unique_high_confidence_required".to_owned(),
        )
        .into());
    }
    if !explicit_selection
        && let Some(session) = preflight
            .candidates
            .first()
            .and_then(|candidate| candidate.suggested_session.as_ref())
        && session
            .evidence
            .iter()
            .any(|evidence| evidence.kind == "exact_thread")
        && matches!(
            session.resume_blocker.as_deref(),
            Some("active_run" | "recovery_required")
        )
    {
        print_route_refusal(&preflight, args.json, args.jsonl)?;
        return Err(skein_core::Error::ControlStateConflict(format!(
            "thread_{}",
            session.resume_blocker.as_deref().unwrap_or("unavailable")
        ))
        .into());
    }
    let (expected_project_id, expected_action, expected_thread_id) =
        if let Some(project_id) = args.project_id {
            let candidate = preflight
                .candidates
                .iter()
                .find(|candidate| candidate.project.id == project_id)
                .ok_or_else(|| {
                    skein_core::Error::InvalidControlRequest(
                        "route_refused:selection_not_ranked".to_owned(),
                    )
                })?;
            if let Some(thread_id) = args.session_id.as_deref()
                && !candidate.suggested_session.as_ref().is_some_and(|session| {
                    session.source_thread_id == thread_id && session.resumable
                })
            {
                return Err(skein_core::Error::InvalidControlRequest(
                    "route_refused:session_not_ranked_or_resumable".to_owned(),
                )
                .into());
            }
            (
                project_id,
                if args.session_id.is_some() {
                    "resume"
                } else {
                    "start"
                }
                .to_owned(),
                args.session_id.clone(),
            )
        } else {
            (
                recommendation.project_id,
                recommendation.action.clone(),
                recommendation.source_thread_id.clone(),
            )
        };
    drop(read_only);

    // Content-free ChatGPT authentication preflight. The detached worker checks again.
    drop(ControlClient::connect()?);
    let mut registry = Registry::open(paths)?;
    let outcome = registry.plan_conductor_run(&skein_core::NewConductorRun {
        request_id: &request_id,
        prompt: &prompt,
        include_session_text: args.include_session_text,
        full_access_acknowledged: true,
        expected: skein_core::ExpectedConductorRoute {
            project_id: expected_project_id,
            action: &expected_action,
            source_thread_id: expected_thread_id.as_deref(),
        },
        explicit_selection,
    })?;
    match outcome {
        skein_core::ConductorPlanOutcome::Created {
            decision,
            control,
            worker,
        } => {
            let snapshot = match worker_runtime::launch_preallocated_worker(
                paths,
                &mut registry,
                worker,
                prompt,
            ) {
                Ok(snapshot) => snapshot,
                Err(_) => {
                    let run = registry
                        .control_run(decision.run_id)?
                        .ok_or("conductor run disappeared after launch failure")?;
                    let worker = registry.control_worker(decision.run_id)?;
                    print_conductor_launch_failure(
                        &decision,
                        &run,
                        worker.as_ref(),
                        args.json,
                        args.jsonl,
                    )?;
                    return Err(skein_core::Error::ControlStateConflict(
                        "worker_launch_failed_after_commit: inspect the durable run before a new attempt"
                            .to_owned(),
                    )
                    .into());
                }
            };
            if args.jsonl {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "type": "route",
                        "decision": decision,
                        "reused": false
                    }))?
                );
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "type": "worker_snapshot",
                        "snapshot": snapshot
                    }))?
                );
            } else if args.json || output::is_json() {
                print_value(
                    &serde_json::json!({
                        "requestId": request_id,
                        "reused": false,
                        "dispatched": true,
                        "decision": decision,
                        "snapshot": snapshot
                    }),
                    true,
                )?;
            } else {
                println!(
                    "routed {} to project {} with high confidence (score {}, margin {})",
                    decision.action, decision.project_id, decision.score, decision.runner_up_margin
                );
                println!(
                    "started reconnectable run {} (request {})",
                    control.run_id, request_id
                );
            }
            if args.follow || args.jsonl {
                let terminal = worker_runtime::watch(paths, control.run_id, args.jsonl)?;
                print_conductor_terminal(&terminal, args.jsonl);
            }
        }
        skein_core::ConductorPlanOutcome::Existing { decision, .. } => {
            let run = registry
                .control_run(decision.run_id)?
                .ok_or("conductor run disappeared")?;
            let worker = registry.control_worker(decision.run_id)?;
            print_conductor_status(
                &decision,
                &run,
                worker.as_ref(),
                true,
                args.json,
                args.jsonl,
            )?;
            if args.follow || args.jsonl {
                let terminal = worker_runtime::watch(paths, decision.run_id, args.jsonl)?;
                print_conductor_terminal(&terminal, args.jsonl);
            }
        }
    }
    Ok(())
}

fn print_route_refusal(
    report: &skein_core::MatchReport,
    json: bool,
    jsonl: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if jsonl {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "type": "route_refused",
                "report": report
            }))?
        );
        Ok(())
    } else {
        print_match_report(report, json)
    }
}

fn print_conductor_status(
    decision: &skein_core::ConductorDecision,
    run: &skein_core::ControlRun,
    worker: Option<&skein_core::ControlWorker>,
    reused: bool,
    json: bool,
    jsonl: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if jsonl {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "type": "route_status",
                "decision": decision,
                "run": run,
                "worker": worker,
                "reused": reused,
                "dispatched": false
            }))?
        );
    } else if json || output::is_json() {
        print_value(
            &serde_json::json!({
                "requestId": decision.request_id,
                "decision": decision,
                "run": run,
                "worker": worker,
                "reused": reused,
                "dispatched": false
            }),
            true,
        )?;
    } else {
        println!(
            "request {} already maps to run {}; prompt was not resubmitted",
            decision.request_id, decision.run_id
        );
        println!("run state: {:?}", run.state);
    }
    Ok(())
}

fn print_conductor_launch_failure(
    decision: &skein_core::ConductorDecision,
    run: &skein_core::ControlRun,
    worker: Option<&skein_core::ControlWorker>,
    json: bool,
    jsonl: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let value = serde_json::json!({
        "type": "worker_launch_failed",
        "requestId": decision.request_id,
        "decision": decision,
        "run": run,
        "worker": worker,
        "errorClass": "worker_launch_failed_after_commit",
        "promptReplayAllowed": false
    });
    if jsonl {
        println!("{}", serde_json::to_string(&value)?);
    } else if json || output::is_json() {
        print_value(&value, true)?;
    } else {
        println!(
            "request {} committed as run {}, but worker launch failed",
            decision.request_id, decision.run_id
        );
        println!("prompt replay is disabled; inspect the durable run before retrying");
    }
    Ok(())
}

fn print_conductor_terminal(run: &skein_core::ControlRun, jsonl: bool) {
    if jsonl {
        if let Ok(value) = serde_json::to_string(&serde_json::json!({
            "type": "worker_run",
            "run": run
        })) {
            println!("{value}");
        }
    } else {
        println!("\nrun {}: {:?}", run.id, run.state);
    }
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
    if (json || output::is_json()) && follow {
        return Err(skein_core::Error::InvalidControlRequest(
            "use --jsonl, not JSON output, when --follow is enabled".to_owned(),
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
    } else if json || output::is_json() {
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

fn read_match_query() -> Result<String, Box<dyn std::error::Error>> {
    let mut query = String::new();
    std::io::stdin()
        .take((MAX_MATCH_QUERY_BYTES + 1) as u64)
        .read_to_string(&mut query)?;
    if query.len() > MAX_MATCH_QUERY_BYTES {
        return Err(skein_core::Error::InvalidControlRequest(format!(
            "match query exceeds the {MAX_MATCH_QUERY_BYTES}-byte limit"
        ))
        .into());
    }
    if query.trim().is_empty() {
        return Err(skein_core::Error::InvalidControlRequest(
            "provide the private match query on standard input".to_owned(),
        )
        .into());
    }
    Ok(query)
}

fn print_match_report(
    report: &skein_core::MatchReport,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if json || output::is_json() {
        return print_value(report, true);
    }
    let Some(recommendation) = &report.recommendation else {
        println!("No metadata match. No project was selected.");
        return Ok(());
    };
    let top = report
        .candidates
        .first()
        .ok_or("match recommendation had no candidate")?;
    println!("Best match: {}", top.project.name);
    println!(
        "Confidence: {} (score {}, runner-up margin {}, dispatch disabled)",
        format!("{:?}", recommendation.confidence).to_lowercase(),
        recommendation.score,
        recommendation.runner_up_margin
    );
    for (index, candidate) in report.candidates.iter().enumerate() {
        println!(
            "{}. {} [{}] (project id {})",
            candidate.rank.max(index + 1),
            candidate.project.name,
            candidate.score,
            candidate.project.id
        );
        for evidence in &candidate.evidence {
            println!(
                "   +{} {}:{} ({} match(es))",
                evidence.points, evidence.family, evidence.kind, evidence.matches
            );
        }
        if let Some(session) = &candidate.suggested_session {
            println!("   session: {}", session.source_thread_id);
            for evidence in &session.evidence {
                println!(
                    "   +{} {}:{} ({} match(es))",
                    evidence.points, evidence.family, evidence.kind, evidence.matches
                );
            }
        }
    }
    if report.resolution.required {
        println!("No work was dispatched. Select one ranked candidate explicitly:");
        println!("  {}", report.resolution.cli_template);
    }
    Ok(())
}

fn local_day_bounds(
    requested: Option<&str>,
) -> Result<(String, String, i64, i64), Box<dyn std::error::Error>> {
    let date = requested.map_or_else(
        || Ok(Local::now().date_naive()),
        |value| NaiveDate::parse_from_str(value, "%Y-%m-%d"),
    )?;
    let next = date
        .succ_opt()
        .ok_or("requested date has no representable following day")?;
    let start = Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0).ok_or("invalid day start")?)
        .earliest()
        .ok_or("local midnight was not representable")?;
    let end = Local
        .from_local_datetime(&next.and_hms_opt(0, 0, 0).ok_or("invalid day end")?)
        .earliest()
        .ok_or("following local midnight was not representable")?;
    let timezone = iana_time_zone::get_timezone().unwrap_or_else(|_| "local".to_owned());
    Ok((
        date.format("%Y-%m-%d").to_string(),
        timezone,
        start.timestamp(),
        end.timestamp(),
    ))
}

fn unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
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
    let json = args.json;
    let include_text = args.include_text;
    let progress = progress::Progress::cli(json);
    progress.stage("requesting bounded Codex thread metadata");
    let report = sync_codex_catalog(paths, &args)?;
    progress.stage("Codex session catalog committed");
    if include_text && !json && !output::is_json() {
        eprintln!(
            "warning: storing Codex thread names and first-message previews in private local state"
        );
    }
    print_value(&report, json)
}

fn sync_codex_catalog(
    paths: &SkeinPaths,
    args: &CodexSyncArgs,
) -> Result<CodexSyncReport, Box<dyn std::error::Error>> {
    let options = DiscoveryOptions {
        limit: args.limit,
        cursor: args.cursor.clone(),
        use_state_db_only: !args.repair_source_index,
        include_text: args.include_text,
    };
    let (threads, next_cursor, repaired_source_index, page_count, complete) = if args.all_pages {
        let result = skein_codex::discover_all(
            &options,
            &DiscoveryBounds {
                max_pages: args.max_pages,
                max_threads: usize::try_from(args.max_threads)?,
            },
        )?;
        (
            result.threads,
            result.next_cursor,
            result.repaired_source_index,
            result.page_count,
            result.complete,
        )
    } else {
        let page = skein_codex::discover(&options)?;
        let complete = page.next_cursor.is_none();
        (
            page.threads,
            page.next_cursor,
            page.repaired_source_index,
            1,
            complete,
        )
    };
    let window_start_at = args
        .since_days
        .map(|days| unix_timestamp().saturating_sub(i64::from(days).saturating_mul(24 * 60 * 60)));
    let observations = threads
        .into_iter()
        .filter(|thread| window_start_at.is_none_or(|start| thread.updated_at >= start))
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
    let source_threads_selected = observations.len();
    let mut registry = Registry::open(paths)?;
    let import = registry.import_sessions(&observations)?;
    Ok(CodexSyncReport {
        import,
        next_cursor,
        repaired_source_index,
        text_imported: args.include_text,
        since_days: args.since_days,
        window_start_at,
        source_threads_selected,
        page_count,
        complete,
        observed_at: unix_timestamp(),
    })
}

pub(crate) fn sync_codex_catalog_default(
    paths: &SkeinPaths,
) -> Result<CodexSyncReport, Box<dyn std::error::Error>> {
    sync_codex_catalog(
        paths,
        &CodexSyncArgs {
            limit: 100,
            cursor: None,
            all_pages: true,
            max_pages: 100,
            max_threads: 10_000,
            include_text: false,
            repair_source_index: false,
            since_days: None,
            json: false,
        },
    )
}

pub(crate) fn refresh_git_resilient(
    registry: &Registry,
    working_tree: bool,
    force: bool,
) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
    let projects = registry.list_projects()?;
    Ok(projects
        .into_iter()
        .map(
            |project| match registry.refresh_project(&project.path, working_tree, force) {
                Ok(report) => value_with_ok(report),
                Err(error) => serde_json::json!({
                    "ok": false,
                    "projectPath": project.path,
                    "error": error.to_string()
                }),
            },
        )
        .collect())
}

pub(crate) fn refresh_documents_resilient(
    registry: &mut Registry,
) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
    let projects = registry.list_projects()?;
    Ok(projects
        .into_iter()
        .map(
            |project| match registry.refresh_project_documents(&project.path) {
                Ok(report) => value_with_ok(report),
                Err(error) => serde_json::json!({
                    "ok": false,
                    "projectPath": project.path,
                    "error": error.to_string()
                }),
            },
        )
        .collect())
}

fn value_with_ok(value: impl Serialize) -> serde_json::Value {
    let mut value = serde_json::to_value(value).unwrap_or_else(|error| {
        serde_json::json!({"error": format!("could not serialize refresh report: {error}")})
    });
    if let Some(object) = value.as_object_mut() {
        object.insert("ok".to_owned(), serde_json::Value::Bool(true));
    }
    value
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

pub(crate) fn codex_home(
    override_home: Option<PathBuf>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = override_home {
        return Ok(path);
    }
    if let Some(path) = std::env::var_os("CODEX_HOME") {
        return Ok(PathBuf::from(path));
    }
    directories::BaseDirs::new()
        .map(|base| base.home_dir().join(".codex"))
        .ok_or_else(|| "could not determine CODEX_HOME".into())
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
    output::print(value, json)
}

#[cfg(test)]
mod local_time_tests {
    use super::*;

    #[test]
    fn local_day_bounds_are_calendar_based_and_bounded() {
        let (date, timezone, start, end) =
            local_day_bounds(Some("2026-07-11")).expect("valid local day");
        assert_eq!(date, "2026-07-11");
        assert!(!timezone.is_empty());
        assert!((23 * 60 * 60..=25 * 60 * 60).contains(&(end - start)));
        assert!(local_day_bounds(Some("not-a-date")).is_err());
    }
}
