use std::path::PathBuf;

use clap::ArgGroup;
use clap::Args;
use clap::Parser;
use clap::Subcommand;
use serde::Serialize;
use skein_codex::DiscoveryOptions;
use skein_core::Registry;
use skein_core::SessionImportReport;
use skein_core::SessionObservation;
use skein_core::SkeinPaths;

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
