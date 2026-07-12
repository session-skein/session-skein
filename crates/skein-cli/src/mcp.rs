//! On-demand MCP stdio adapter over Session Skein's existing core and control paths.

use std::borrow::Cow;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use rmcp::ErrorData as McpError;
use rmcp::ServiceExt;
use rmcp::handler::server::ServerHandler;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResult;
use rmcp::model::ListToolsResult;
use rmcp::model::PaginatedRequestParams;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerInfo;
use rmcp::model::Tool;
use rmcp::model::ToolAnnotations;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use skein_core::ControlRunState;
use skein_core::MatchOptions;
use skein_core::Project;
use skein_core::Registry;
use skein_core::ScanRootOptions;
use skein_core::SessionMetadata;
use skein_core::SkeinPaths;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;
use uuid::Uuid;

use crate::worker_runtime;

const MAX_TOOL_ARGUMENT_BYTES: usize = 128 * 1024;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_CHILD_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_LIST_LIMIT: usize = 500;
const CHILD_TIMEOUT: Duration = Duration::from_secs(120);

const READ_ONLY_INSTRUCTIONS: &str = "Session Skein is the local Codex project/session catalog. Use search_projects before guessing a path. Empty search results with setupRequired=true mean you must ask which root the user approves before calling add_scan_root. Recursive discovery is an explicit opt-in. Deep recall requires both an enabled private source and include_deep_context=true on each search; admitted text may contain prompts, diffs, commands, or credentials and returned snippets enter the model context. This server was started without Codex worker-control authority. Default metadata/control paths do not intentionally store content or credentials; opted-in recall stores bounded private source text.";
const CONTROL_INSTRUCTIONS: &str = "Session Skein is the local Codex project/session control plane. Use search_projects before guessing a path; weak or ambiguous matches are not dispatch authority. Deep recall requires both an enabled private source and include_deep_context=true on each search; admitted text may contain sensitive content and returned snippets enter the model context. This server was explicitly started with worker-control authority. Before conduct, steer_run, interrupt_run, or reconcile_run, confirm intent. conduct requires full_access_acknowledged=true and a caller UUID. Never retry a lost prompt under a new request_id.";

#[derive(Clone)]
struct SkeinMcpServer {
    paths: SkeinPaths,
    tools: Arc<Vec<Tool>>,
    allow_control: bool,
}

impl SkeinMcpServer {
    fn new(paths: SkeinPaths, allow_control: bool) -> Self {
        Self {
            paths,
            tools: Arc::new(tool_catalog(allow_control)),
            allow_control,
        }
    }
}

impl ServerHandler for SkeinMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            if self.allow_control {
                CONTROL_INSTRUCTIONS
            } else {
                READ_ONLY_INSTRUCTIONS
            },
        )
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        let tools = self.tools.clone();
        async move {
            Ok(ListToolsResult {
                tools: (*tools).clone(),
                next_cursor: None,
                meta: None,
            })
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let name = request.name.into_owned();
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let argument_bytes = serde_json::to_vec(&arguments)
            .map_err(|error| McpError::invalid_params(error.to_string(), None))?
            .len();
        if argument_bytes > MAX_TOOL_ARGUMENT_BYTES {
            return Err(McpError::invalid_params(
                "tool argument envelope exceeds 128 KiB".to_owned(),
                None,
            ));
        }
        let result = execute_tool(self.paths.clone(), name, arguments, self.allow_control)
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        Ok(match result {
            Ok(value) => CallToolResult::structured(value),
            Err(message) => CallToolResult::structured_error(json!({
                "ok": false,
                "error": {
                    "code": "tool_failed",
                    "message": message
                }
            })),
        })
    }
}

pub fn run(paths: SkeinPaths, allow_control: bool) -> Result<(), Box<dyn std::error::Error>> {
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err("skein mcp requires MCP stdio, not an interactive terminal".into());
    }
    // Initialize or migrate owner-private state at server startup so the first
    // read-only tool can return onboarding guidance on a fresh installation.
    drop(Registry::open(&paths)?);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let service = SkeinMcpServer::new(paths, allow_control);
        let running = service
            .serve((tokio::io::stdin(), tokio::io::stdout()))
            .await?;
        running.waiting().await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })
}

fn tool_catalog(allow_control: bool) -> Vec<Tool> {
    let mut tools = vec![
        tool(
            "search_projects",
            "Search registered project/session metadata plus bounded identity documents. Set include_deep_context=true only with user intent to include enabled private memory/session snippets in model context.",
            json!({
                "query": {"type": "string", "minLength": 1, "maxLength": 65536},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50, "default": 10},
                "include_session_text": {"type": "boolean", "default": false},
                "include_deep_context": {"type": "boolean", "default": false}
            }),
            &["query"],
            true,
        ),
        tool(
            "get_project",
            "Get a deterministic project card by numeric ID, exact registered path, or exact project name.",
            json!({"project_id_or_path": {"type": "string", "minLength": 1}}),
            &["project_id_or_path"],
            true,
        ),
        tool(
            "suggest_codex_command",
            "Suggest a codex -C command only for an unambiguous match. Set include_deep_context=true only with user intent to route from enabled private memory/session snippets.",
            json!({
                "query": {"type": "string", "minLength": 1, "maxLength": 65536},
                "include_deep_context": {"type": "boolean", "default": false}
            }),
            &["query"],
            true,
        ),
        tool(
            "list_projects",
            "List generated-on-read cards for all explicitly registered projects.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "list_scan_roots",
            "List user-approved repository discovery roots and whether each root is recursive.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "list_sessions",
            "List content-free Codex session metadata, optionally limited to one project.",
            json!({
                "project": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 100}
            }),
            &[],
            true,
        ),
        tool(
            "list_runs",
            "List redaction-safe audited control runs, optionally filtered by project and open/recovery state.",
            json!({
                "project": {"type": "string"},
                "active_only": {"type": "boolean", "default": false},
                "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 100}
            }),
            &[],
            true,
        ),
        tool(
            "get_run",
            "Get one run's redaction-safe policy, turn, actions, events, and worker record.",
            json!({"run_id": {"type": "integer", "minimum": 1}}),
            &["run_id"],
            true,
        ),
        tool(
            "get_day_summary",
            "Return a deterministic metadata-only activity digest for a local calendar day.",
            json!({"date": {"type": "string", "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}$"}}),
            &[],
            true,
        ),
        tool(
            "get_recent_activity",
            "Return recent redaction-safe control runs. This compatibility view never reads raw Codex transcripts.",
            json!({
                "hours": {"type": "number", "minimum": 0, "maximum": 87600, "default": 24},
                "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 50},
                "project": {"type": "string"}
            }),
            &[],
            true,
        ),
        tool(
            "get_activity_status",
            "Return local catalog counts and latest redaction-safe activity timestamps.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "get_context_settings",
            "Describe the defaults-off memory and raw-session recall settings and their private local storage scope.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "set_codex_memory_indexing",
            "Enable or disable private generated Codex memory-summary recall. A later refresh_index applies the setting.",
            json!({"enabled": {"type": "boolean"}}),
            &["enabled"],
            false,
        ),
        tool(
            "set_codex_session_indexing",
            "Enable or disable private raw Codex user/assistant session recall beneath approved scan roots. A later refresh_index applies the setting.",
            json!({"enabled": {"type": "boolean"}}),
            &["enabled"],
            false,
        ),
        tool(
            "add_project",
            "Register exactly one existing project directory. This does not crawl its parent or scan file contents.",
            json!({
                "path": {"type": "string", "minLength": 1},
                "name": {"type": "string", "minLength": 1}
            }),
            &["path"],
            false,
        ),
        tool(
            "add_scan_root",
            "Add a user-approved discovery root and immediately index Git repositories. Recursive discovery is explicit and optional.",
            json!({
                "path": {"type": "string", "minLength": 1},
                "recursive": {"type": "boolean", "default": false},
                "max_depth": {"type": "integer", "minimum": 0, "maximum": 64}
            }),
            &["path"],
            false,
        ),
        tool(
            "remove_scan_root",
            "Remove a discovery root without deleting projects already discovered from it. The root may be unmounted or missing.",
            json!({"path": {"type": "string", "minLength": 1}}),
            &["path"],
            false,
        ),
        tool(
            "refresh_index",
            "Refresh all configured sources, exactly one registered project, or exactly one configured scan root. Project and scan_root are mutually exclusive; scoped refreshes defer global context and session sources.",
            json!({
                "working_tree": {"type": "boolean", "default": false},
                "force": {"type": "boolean", "default": false},
                "project": {"type": "string", "minLength": 1},
                "scan_root": {"type": "string", "minLength": 1}
            }),
            &[],
            false,
        ),
        tool(
            "refresh_activity",
            "Compatibility refresh: synchronize bounded content-free Codex session metadata and optionally refresh bounded Git metadata. Raw transcripts are never read.",
            json!({
                "since_days": {"type": "integer", "minimum": 0, "maximum": 3650, "default": 7},
                "include_git": {"type": "boolean", "default": false},
                "session_limit": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 100},
                "max_pages": {"type": "integer", "minimum": 1, "maximum": 100, "default": 100},
                "max_threads": {"type": "integer", "minimum": 1, "maximum": 10000, "default": 10000}
            }),
            &[],
            false,
        ),
        tool(
            "sync_codex_sessions",
            "Synchronize bounded content-free session metadata from the installed Codex app-server, following all cursors by default.",
            json!({
                "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 100},
                "all_pages": {"type": "boolean", "default": true},
                "max_pages": {"type": "integer", "minimum": 1, "maximum": 100, "default": 100},
                "max_threads": {"type": "integer", "minimum": 1, "maximum": 10000, "default": 10000}
            }),
            &[],
            false,
        ),
        tool(
            "conduct",
            "Route one private prompt. Automatic dispatch requires a unique high-confidence match; an exact project_id and optional source_thread_id from a prior ranked result explicitly resolve ambiguity without reinterpretation. Requires explicit full-access acknowledgement.",
            json!({
                "prompt": {"type": "string", "minLength": 1, "maxLength": 65536},
                "full_access_acknowledged": {"type": "boolean", "const": true},
                "request_id": {"type": "string", "format": "uuid"},
                "include_session_text": {"type": "boolean", "default": false},
                "project_id": {"type": "integer", "minimum": 1},
                "source_thread_id": {"type": "string", "minLength": 1}
            }),
            &["prompt", "full_access_acknowledged", "request_id"],
            false,
        ),
        tool(
            "steer_run",
            "Queue text onto the exact active turn owned by a Skein worker. The content remains in authenticated IPC and Codex memory only.",
            json!({
                "run_id": {"type": "integer", "minimum": 1},
                "prompt": {"type": "string", "minLength": 1, "maxLength": 65536},
                "request_id": {"type": "string", "format": "uuid"}
            }),
            &["run_id", "prompt", "request_id"],
            false,
        ),
        tool(
            "interrupt_run",
            "Interrupt the exact active Codex turn owned by the selected Skein run.",
            json!({"run_id": {"type": "integer", "minimum": 1}}),
            &["run_id"],
            false,
        ),
        tool(
            "reconcile_run",
            "Read and reconcile the exact recorded Codex turn for a recovery-required run without replay or takeover.",
            json!({
                "run_id": {"type": "integer", "minimum": 1},
                "request_id": {"type": "string", "format": "uuid"}
            }),
            &["run_id", "request_id"],
            false,
        ),
    ];
    if !allow_control {
        tools.retain(|tool| !control_tool(tool.name.as_ref()));
    }
    tools
}

fn control_tool(name: &str) -> bool {
    matches!(
        name,
        "conduct" | "steer_run" | "interrupt_run" | "reconcile_run"
    )
}

fn tool(
    name: &'static str,
    description: &'static str,
    properties: Value,
    required: &[&str],
    read_only: bool,
) -> Tool {
    let schema = json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    });
    let schema = schema.as_object().cloned().unwrap_or_default();
    let mut tool = Tool::new(
        Cow::Borrowed(name),
        Cow::Borrowed(description),
        Arc::new(schema),
    );
    let destructive = matches!(
        name,
        "conduct" | "steer_run" | "interrupt_run" | "remove_scan_root"
    );
    let open_world = matches!(
        name,
        "sync_codex_sessions" | "refresh_activity" | "conduct" | "steer_run"
    );
    let idempotent = read_only
        || matches!(
            name,
            "set_codex_memory_indexing"
                | "set_codex_session_indexing"
                | "add_project"
                | "add_scan_root"
                | "refresh_index"
                | "refresh_activity"
                | "sync_codex_sessions"
                | "conduct"
                | "steer_run"
                | "reconcile_run"
        );
    tool.annotations = Some(
        ToolAnnotations::new()
            .read_only(read_only)
            .destructive(destructive)
            .idempotent(idempotent)
            .open_world(open_world),
    );
    tool
}

async fn execute_tool(
    paths: SkeinPaths,
    name: String,
    arguments: Value,
    allow_control: bool,
) -> Result<Result<Value, String>, String> {
    if control_tool(&name) && !allow_control {
        return Ok(Err(
            "Codex worker control is disabled; register the MCP command with `skein mcp --allow-control` to expose these tools"
                .to_owned(),
        ));
    }
    match name.as_str() {
        "refresh_activity" => Ok(match parse(arguments) {
            Ok(args) => refresh_activity(&paths, args).await,
            Err(error) => Err(error),
        }),
        "sync_codex_sessions" => Ok(match parse(arguments) {
            Ok(args) => sync_codex_sessions(args).await,
            Err(error) => Err(error),
        }),
        "conduct" => Ok(match parse(arguments) {
            Ok(args) => conduct(args).await,
            Err(error) => Err(error),
        }),
        _ => tokio::task::spawn_blocking(move || execute_local_tool(&paths, &name, arguments))
            .await
            .map_err(|_| "tool task failed".to_owned()),
    }
}

fn execute_local_tool(paths: &SkeinPaths, name: &str, arguments: Value) -> Result<Value, String> {
    match name {
        "search_projects" => search_projects(paths, parse(arguments)?),
        "get_project" => get_project(paths, parse(arguments)?),
        "suggest_codex_command" => suggest_codex_command(paths, parse(arguments)?),
        "list_projects" => list_projects(paths),
        "list_scan_roots" => list_scan_roots(paths),
        "list_sessions" => list_sessions(paths, parse(arguments)?),
        "list_runs" => list_runs(paths, parse(arguments)?),
        "get_run" => get_run(paths, parse(arguments)?),
        "get_day_summary" => get_day_summary(paths, parse(arguments)?),
        "get_recent_activity" => get_recent_activity(paths, parse(arguments)?),
        "get_activity_status" => get_activity_status(paths),
        "get_context_settings" => context_settings(paths),
        "set_codex_memory_indexing" => set_context_indexing(paths, parse(arguments)?, "memory"),
        "set_codex_session_indexing" => set_context_indexing(paths, parse(arguments)?, "session"),
        "add_project" => add_project(paths, parse(arguments)?, false),
        "add_scan_root" => add_scan_root(paths, parse(arguments)?),
        "remove_scan_root" => remove_scan_root(paths, parse(arguments)?),
        "refresh_index" => refresh_index(paths, parse(arguments)?),
        "refresh_activity" | "sync_codex_sessions" | "conduct" => {
            Err(format!("async tool reached blocking dispatcher: {name}"))
        }
        "steer_run" => steer_run(paths, parse(arguments)?),
        "interrupt_run" => interrupt_run(paths, parse(arguments)?),
        "reconcile_run" => reconcile_run(paths, parse(arguments)?),
        _ => Err(format!("unknown tool: {name}")),
    }
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default = "default_match_limit")]
    limit: usize,
    #[serde(default)]
    include_session_text: bool,
    #[serde(default)]
    include_deep_context: bool,
}

#[derive(Deserialize)]
struct ProjectArgs {
    project_id_or_path: String,
}

#[derive(Deserialize)]
struct QueryArgs {
    query: String,
    #[serde(default)]
    include_deep_context: bool,
}

#[derive(Default, Deserialize)]
struct ListArgs {
    project: Option<String>,
    limit: Option<usize>,
    #[serde(default)]
    active_only: bool,
}

#[derive(Deserialize)]
struct RunArgs {
    run_id: i64,
}

#[derive(Default, Deserialize)]
struct DayArgs {
    date: Option<String>,
}

#[derive(Deserialize)]
struct RecentArgs {
    #[serde(default = "default_activity_hours")]
    hours: f64,
    #[serde(default = "default_activity_limit")]
    limit: usize,
    project: Option<String>,
}

#[derive(Deserialize)]
struct AddProjectArgs {
    path: String,
    name: Option<String>,
}

#[derive(Deserialize)]
struct PathArgs {
    path: String,
}

#[derive(Deserialize)]
struct AddScanRootArgs {
    path: String,
    #[serde(default)]
    recursive: bool,
    max_depth: Option<u16>,
}

#[derive(Deserialize)]
struct SetIndexingArgs {
    enabled: bool,
}

#[derive(Default, Deserialize)]
struct RefreshArgs {
    #[serde(default)]
    working_tree: bool,
    #[serde(default)]
    force: bool,
    project: Option<String>,
    scan_root: Option<String>,
}

#[derive(Deserialize)]
struct RefreshActivityArgs {
    #[serde(default = "default_since_days")]
    since_days: usize,
    #[serde(default)]
    include_git: bool,
    #[serde(default = "default_sync_limit")]
    session_limit: usize,
    #[serde(default = "default_max_pages")]
    max_pages: u32,
    #[serde(default = "default_max_threads")]
    max_threads: usize,
}

#[derive(Deserialize)]
struct SyncArgs {
    #[serde(default = "default_sync_limit")]
    limit: usize,
    #[serde(default = "default_true")]
    all_pages: bool,
    #[serde(default = "default_max_pages")]
    max_pages: u32,
    #[serde(default = "default_max_threads")]
    max_threads: usize,
}

#[derive(Deserialize)]
struct ConductArgs {
    prompt: String,
    full_access_acknowledged: bool,
    request_id: String,
    #[serde(default)]
    include_session_text: bool,
    project_id: Option<i64>,
    source_thread_id: Option<String>,
}

#[derive(Deserialize)]
struct SteerArgs {
    run_id: i64,
    prompt: String,
    request_id: String,
}

#[derive(Deserialize)]
struct ReconcileArgs {
    run_id: i64,
    request_id: String,
}

fn parse<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| format!("invalid tool arguments: {error}"))
}

fn search_projects(paths: &SkeinPaths, args: SearchArgs) -> Result<Value, String> {
    validate_query(&args.query)?;
    if !(1..=50).contains(&args.limit) {
        return Err("limit must be in 1..=50".to_owned());
    }
    let registry = Registry::open_read_only(paths).map_err(error_string)?;
    let setup_required = registry.list_scan_roots().map_err(error_string)?.is_empty()
        && registry.list_projects().map_err(error_string)?.is_empty();
    let report = registry
        .match_metadata(&MatchOptions {
            query: &args.query,
            include_text: args.include_session_text,
            limit: args.limit,
            now: unix_timestamp(),
        })
        .map_err(error_string)?;
    let document_matches = registry
        .search_project_documents(&args.query, args.limit)
        .map_err(error_string)?;
    let context_matches = if args.include_deep_context {
        registry
            .search_context_documents(&args.query, args.limit)
            .map_err(error_string)?
    } else {
        Vec::new()
    };
    let settings = registry.get_recall_settings().map_err(error_string)?;
    let freshness = registry
        .catalog_freshness(unix_timestamp(), skein_core::DEFAULT_STALE_AFTER_SECONDS)
        .map_err(error_string)?;
    let private_authorized = settings.include_codex_memories || settings.include_codex_sessions;
    let mut sources_consulted = vec!["project_metadata", "project_documents"];
    if args.include_deep_context && settings.include_codex_memories {
        sources_consulted.push("codex_memory");
    }
    if args.include_deep_context && settings.include_codex_sessions {
        sources_consulted.push("codex_session");
    }
    let context_truncated = context_matches.len() == args.limit;
    Ok(json!({
        "ok": true,
        "setupRequired": setup_required,
        "setup_required": setup_required,
        "setupHint": setup_required.then_some("Ask the user which directory may be indexed, call add_scan_root (recursive=true only when nested repositories should be discovered), then call refresh_index."),
        "documentMatches": document_matches,
        "contextMatches": context_matches,
        "recall": {
            "mode": if args.include_deep_context { "deep_private" } else { "quick_indexed" },
            "sourcesConsulted": sources_consulted,
            "privateContextAuthorized": private_authorized,
            "privateSources": settings,
            "contextFreshness": freshness.context,
            "limit": args.limit,
            "contextReturned": context_matches.len(),
            "contextPossiblyTruncated": context_truncated,
            "escalationSuggested": !args.include_deep_context && private_authorized,
            "escalationReason": (!args.include_deep_context && private_authorized).then_some("quick recall did not consult enabled private context"),
            "nextTool": (!args.include_deep_context).then_some(json!({
                "name": "search_projects",
                "arguments": {"query": "repeat the same private query", "include_deep_context": true}
            }))
        },
        "report": report
    }))
}

fn get_project(paths: &SkeinPaths, args: ProjectArgs) -> Result<Value, String> {
    let registry = Registry::open_read_only(paths).map_err(error_string)?;
    let project = resolve_project(&registry, &args.project_id_or_path)?;
    let card = registry.project_card(&project.path).map_err(error_string)?;
    Ok(json!({"ok": true, "project": card}))
}

fn suggest_codex_command(paths: &SkeinPaths, args: QueryArgs) -> Result<Value, String> {
    validate_query(&args.query)?;
    let registry = Registry::open_read_only(paths).map_err(error_string)?;
    let report = registry
        .match_metadata(&MatchOptions {
            query: &args.query,
            include_text: false,
            limit: 5,
            now: unix_timestamp(),
        })
        .map_err(error_string)?;
    let command = report.recommendation.as_ref().and_then(|recommendation| {
        if recommendation.ambiguous {
            return None;
        }
        report
            .candidates
            .iter()
            .find(|candidate| candidate.project.id == recommendation.project_id)
            .map(|candidate| {
                let path = candidate.project.path.to_string_lossy().into_owned();
                json!({
                    "argv": ["codex", "-C", path],
                    "shell": format!("codex -C {}", shell_quote(&candidate.project.path))
                })
            })
    });
    let document_matches = registry
        .search_project_documents(&args.query, 5)
        .map_err(error_string)?;
    let context_matches = if args.include_deep_context {
        registry
            .search_context_documents(&args.query, 5)
            .map_err(error_string)?
    } else {
        Vec::new()
    };
    let command = command
        .or_else(|| {
            (document_matches.len() == 1).then(|| {
                let hit = &document_matches[0];
                json!({
                    "argv": ["codex", "-C", hit.project_path],
                    "shell": format!("codex -C {}", shell_quote(&hit.project_path)),
                    "reason": "unique_project_document_match"
                })
            })
        })
        .or_else(|| {
            let mut paths = context_matches
                .iter()
                .filter_map(|hit| hit.project_path.as_ref())
                .collect::<Vec<_>>();
            paths.sort();
            paths.dedup();
            (paths.len() == 1).then(|| {
                let path = paths[0];
                json!({
                    "argv": ["codex", "-C", path],
                    "shell": format!("codex -C {}", shell_quote(path)),
                    "reason": "unique_context_document_match"
                })
            })
        });
    Ok(json!({
        "ok": true,
        "command": command,
        "documentMatches": document_matches,
        "contextMatches": context_matches,
        "report": report
    }))
}

fn list_projects(paths: &SkeinPaths) -> Result<Value, String> {
    let registry = Registry::open_read_only(paths).map_err(error_string)?;
    Ok(json!({
        "ok": true,
        "projects": registry.project_cards().map_err(error_string)?
    }))
}

fn list_scan_roots(paths: &SkeinPaths) -> Result<Value, String> {
    let registry = Registry::open_read_only(paths).map_err(error_string)?;
    let roots = registry.list_scan_roots().map_err(error_string)?;
    Ok(json!({
        "ok": true,
        "roots": roots,
        "recursiveScanningAvailable": true
    }))
}

fn list_sessions(paths: &SkeinPaths, args: ListArgs) -> Result<Value, String> {
    let registry = Registry::open_read_only(paths).map_err(error_string)?;
    let project_id = resolve_optional_project_id(&registry, args.project.as_deref())?;
    let limit = validate_list_limit(args.limit.unwrap_or(100))?;
    let sessions = registry
        .list_session_metadata()
        .map_err(error_string)?
        .into_iter()
        .filter(|session| project_id.is_none() || session.project_id == project_id)
        .take(limit)
        .map(session_value)
        .collect::<Vec<_>>();
    Ok(json!({"ok": true, "sessions": sessions, "contentRedacted": true}))
}

fn list_runs(paths: &SkeinPaths, args: ListArgs) -> Result<Value, String> {
    let registry = Registry::open_read_only(paths).map_err(error_string)?;
    let project_id = resolve_optional_project_id(&registry, args.project.as_deref())?;
    let limit = validate_list_limit(args.limit.unwrap_or(100))?;
    let runs = registry
        .list_control_runs()
        .map_err(error_string)?
        .into_iter()
        .filter(|run| project_id.is_none() || Some(run.project_id) == project_id)
        .filter(|run| !args.active_only || open_run(run.state))
        .take(limit)
        .collect::<Vec<_>>();
    Ok(json!({"ok": true, "runs": runs, "contentRedacted": true}))
}

fn get_run(paths: &SkeinPaths, args: RunArgs) -> Result<Value, String> {
    let registry = Registry::open_read_only(paths).map_err(error_string)?;
    let detail = registry
        .control_run_detail(args.run_id)
        .map_err(error_string)?;
    let worker = registry.control_worker(args.run_id).map_err(error_string)?;
    Ok(json!({
        "ok": true,
        "detail": detail,
        "worker": worker,
        "contentRedacted": true
    }))
}

fn get_day_summary(paths: &SkeinPaths, args: DayArgs) -> Result<Value, String> {
    let registry = Registry::open_read_only(paths).map_err(error_string)?;
    let (date, timezone, start_at, end_at) =
        crate::local_day_bounds(args.date.as_deref()).map_err(|error| error.to_string())?;
    let summary = registry
        .day_summary(&date, &timezone, start_at, end_at)
        .map_err(error_string)?;
    Ok(json!({"ok": true, "summary": summary}))
}

fn get_recent_activity(paths: &SkeinPaths, args: RecentArgs) -> Result<Value, String> {
    if !args.hours.is_finite() || !(0.0..=87_600.0).contains(&args.hours) {
        return Err("hours must be finite and in 0..=87600".to_owned());
    }
    let limit = validate_list_limit(args.limit)?;
    let registry = Registry::open_read_only(paths).map_err(error_string)?;
    let project_id = resolve_optional_project_id(&registry, args.project.as_deref())?;
    let threshold = unix_timestamp().saturating_sub((args.hours * 3600.0) as i64);
    let runs = registry
        .list_control_runs()
        .map_err(error_string)?
        .into_iter()
        .filter(|run| run.updated_at >= threshold)
        .filter(|run| project_id.is_none() || Some(run.project_id) == project_id)
        .take(limit)
        .collect::<Vec<_>>();
    Ok(json!({
        "ok": true,
        "runs": runs,
        "coverage": "durable_control_runs_only",
        "rawTranscriptsRead": false,
        "contentRedacted": true
    }))
}

fn get_activity_status(paths: &SkeinPaths) -> Result<Value, String> {
    let registry = Registry::open_read_only(paths).map_err(error_string)?;
    let projects = registry.list_projects().map_err(error_string)?;
    let sessions = registry.list_session_metadata().map_err(error_string)?;
    let runs = registry.list_control_runs().map_err(error_string)?;
    let freshness = registry
        .catalog_freshness(unix_timestamp(), skein_core::DEFAULT_STALE_AFTER_SECONDS)
        .map_err(error_string)?;
    Ok(json!({
        "ok": true,
        "projects": projects.len(),
        "sessions": sessions.len(),
        "runs": runs.len(),
        "latestSessionObservedAt": sessions.iter().map(|item| item.last_seen_at).max(),
        "latestRunUpdatedAt": runs.iter().map(|item| item.updated_at).max(),
        "catalogFreshness": freshness,
        "rawTranscriptsRead": false
    }))
}

fn context_settings(paths: &SkeinPaths) -> Result<Value, String> {
    let registry = Registry::open_read_only(paths).map_err(error_string)?;
    let settings = registry.get_recall_settings().map_err(error_string)?;
    Ok(json!({
        "ok": true,
        "codexRuntime": "installed_cli_app_server",
        "rawTranscriptIndexing": settings.include_codex_sessions,
        "generatedMemoryIndexing": settings.include_codex_memories,
        "sessionTextStoredByDefault": false,
        "deepRecallDefault": false,
        "note": "Deep recall is explicit. Raw sessions are restricted to approved scan roots and only user/assistant message text is admitted."
    }))
}

fn set_context_indexing(
    paths: &SkeinPaths,
    args: SetIndexingArgs,
    kind: &str,
) -> Result<Value, String> {
    let registry = Registry::open(paths).map_err(error_string)?;
    let mut settings = registry.get_recall_settings().map_err(error_string)?;
    match kind {
        "memory" => settings.include_codex_memories = args.enabled,
        "session" => settings.include_codex_sessions = args.enabled,
        _ => return Err("unknown context source".to_owned()),
    }
    let settings = registry
        .set_recall_settings(settings)
        .map_err(error_string)?;
    Ok(json!({
        "ok": true,
        "enabled": args.enabled,
        "setting": kind,
        "settings": settings,
        "refreshRequired": true
    }))
}

fn add_project(
    paths: &SkeinPaths,
    args: AddProjectArgs,
    compatibility_alias: bool,
) -> Result<Value, String> {
    let registry = Registry::open(paths).map_err(error_string)?;
    let project = registry
        .add_project(Path::new(&args.path), args.name.as_deref())
        .map_err(error_string)?;
    Ok(json!({
        "ok": true,
        "project": project,
        "compatibilityAlias": compatibility_alias,
        "recursiveScanning": false
    }))
}

fn add_scan_root(paths: &SkeinPaths, args: AddScanRootArgs) -> Result<Value, String> {
    if !args.recursive && args.max_depth.is_some() {
        return Err("max_depth requires recursive=true".to_owned());
    }
    let registry = Registry::open(paths).map_err(error_string)?;
    let root = registry
        .add_scan_root(
            Path::new(&args.path),
            ScanRootOptions {
                recursive: args.recursive,
                max_depth: args.max_depth,
            },
        )
        .map_err(error_string)?;
    let discovery = registry
        .discover_scan_root(&root.path)
        .map_err(error_string)?;
    Ok(json!({
        "ok": true,
        "root": root,
        "discovery": discovery,
        "recursiveScanning": args.recursive
    }))
}

fn remove_scan_root(paths: &SkeinPaths, args: PathArgs) -> Result<Value, String> {
    let mut registry = Registry::open(paths).map_err(error_string)?;
    let root = registry
        .remove_scan_root(Path::new(&args.path))
        .map_err(error_string)?;
    Ok(json!({
        "ok": true,
        "removed": true,
        "root": root,
        "discoveredProjectsRetained": true
    }))
}

fn refresh_index(paths: &SkeinPaths, args: RefreshArgs) -> Result<Value, String> {
    let scope = match (args.project, args.scan_root) {
        (Some(_), Some(_)) => {
            return Err("project and scan_root are mutually exclusive".to_owned());
        }
        (Some(path), None) => crate::indexing::IndexScope::Project(path.into()),
        (None, Some(path)) => crate::indexing::IndexScope::ScanRoot(path.into()),
        (None, None) => crate::indexing::IndexScope::All,
    };
    let report = crate::indexing::refresh_index(paths, scope, args.working_tree, args.force)
        .map_err(error_string)?;
    let mut result = serde_json::to_value(report).map_err(error_string)?;
    let object = result
        .as_object_mut()
        .ok_or_else(|| "index report was not an object".to_owned())?;
    object.insert("ok".to_owned(), json!(true));
    object.insert("repositoryContentScanned".to_owned(), json!(true));
    object.insert(
        "repositoryContentScope".to_owned(),
        json!("bounded_identity_documents"),
    );
    object.insert("gitFetchPerformed".to_owned(), json!(false));
    Ok(result)
}

async fn refresh_activity(paths: &SkeinPaths, args: RefreshActivityArgs) -> Result<Value, String> {
    if args.since_days > 3650 {
        return Err("since_days must be in 0..=3650".to_owned());
    }
    validate_pagination_bounds(args.max_pages, args.max_threads)?;
    let limit = args.session_limit.to_string();
    let since_days = args.since_days.to_string();
    let max_pages = args.max_pages.to_string();
    let max_threads = args.max_threads.to_string();
    let sessions = invoke_cli_json(
        &[
            "session",
            "sync",
            "codex",
            "--limit",
            &limit,
            "--since-days",
            &since_days,
            "--all-pages",
            "--max-pages",
            &max_pages,
            "--max-threads",
            &max_threads,
            "--json",
        ],
        None,
    )
    .await?;
    let git = if args.include_git {
        let paths = paths.clone();
        Some(
            tokio::task::spawn_blocking(move || {
                let mut registry = Registry::open(&paths).map_err(error_string)?;
                let discovery = registry.discover_all_scan_roots().map_err(error_string)?;
                let reports =
                    crate::refresh_git_resilient(&registry, false, false).map_err(error_string)?;
                let documents =
                    crate::refresh_documents_resilient(&mut registry).map_err(error_string)?;
                Ok::<Value, String>(json!({
                    "ok": true,
                    "discovery": discovery,
                    "reports": reports,
                    "documents": documents,
                    "contextRefreshed": false
                }))
            })
            .await
            .map_err(|_| "Git refresh task failed".to_owned())??,
        )
    } else {
        None
    };
    Ok(json!({
        "ok": true,
        "sessions": sessions,
        "git": git,
        "sinceDays": args.since_days,
        "coverage": "bounded_app_server_metadata_updated_within_requested_window",
        "rawTranscriptsRead": false
    }))
}

async fn sync_codex_sessions(args: SyncArgs) -> Result<Value, String> {
    if !(1..=1000).contains(&args.limit) {
        return Err("limit must be in 1..=1000".to_owned());
    }
    validate_pagination_bounds(args.max_pages, args.max_threads)?;
    let mut argv = vec![
        "session".to_owned(),
        "sync".to_owned(),
        "codex".to_owned(),
        "--limit".to_owned(),
        args.limit.to_string(),
        "--json".to_owned(),
    ];
    if args.all_pages {
        argv.extend([
            "--all-pages".to_owned(),
            "--max-pages".to_owned(),
            args.max_pages.to_string(),
            "--max-threads".to_owned(),
            args.max_threads.to_string(),
        ]);
    }
    let refs = argv.iter().map(String::as_str).collect::<Vec<_>>();
    invoke_cli_json(&refs, None).await
}

async fn conduct(args: ConductArgs) -> Result<Value, String> {
    validate_query(&args.prompt)?;
    if !args.full_access_acknowledged {
        return Err(
            "full_access_acknowledged must be true for danger-full-access with approvals disabled"
                .to_owned(),
        );
    }
    let request_id = validated_request_id(Some(args.request_id))?;
    let mut argv = vec![
        "conduct".to_owned(),
        "--full-access".to_owned(),
        "--request-id".to_owned(),
        request_id,
        "--json".to_owned(),
    ];
    if args.include_session_text {
        argv.push("--include-session-text".to_owned());
    }
    if let Some(project_id) = args.project_id {
        argv.extend(["--project-id".to_owned(), project_id.to_string()]);
    }
    if let Some(thread_id) = args.source_thread_id {
        if args.project_id.is_none() {
            return Err("source_thread_id requires project_id".to_owned());
        }
        argv.extend(["--session-id".to_owned(), thread_id]);
    }
    let refs = argv.iter().map(String::as_str).collect::<Vec<_>>();
    invoke_cli_json(&refs, Some(args.prompt.as_bytes())).await
}

fn steer_run(paths: &SkeinPaths, args: SteerArgs) -> Result<Value, String> {
    validate_query(&args.prompt)?;
    let request_id = validated_request_id(Some(args.request_id))?;
    let (action_id, queued) = worker_runtime::steer(paths, args.run_id, &request_id, args.prompt)
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "ok": true,
        "runId": args.run_id,
        "requestId": request_id,
        "actionId": action_id,
        "queued": queued,
        "contentPersisted": false
    }))
}

fn interrupt_run(paths: &SkeinPaths, args: RunArgs) -> Result<Value, String> {
    worker_runtime::interrupt(paths, args.run_id).map_err(|error| error.to_string())?;
    Ok(json!({"ok": true, "runId": args.run_id, "interruptQueued": true}))
}

fn reconcile_run(paths: &SkeinPaths, args: ReconcileArgs) -> Result<Value, String> {
    let request_id = validated_request_id(Some(args.request_id))?;
    let value = worker_runtime::reconcile(paths, args.run_id, &request_id)
        .map_err(|error| error.to_string())?;
    Ok(json!({"ok": true, "result": value, "contentRedacted": true}))
}

fn resolve_optional_project_id(
    registry: &Registry,
    value: Option<&str>,
) -> Result<Option<i64>, String> {
    value
        .map(|value| resolve_project(registry, value).map(|project| project.id))
        .transpose()
}

fn resolve_project(registry: &Registry, value: &str) -> Result<Project, String> {
    let projects = registry.list_projects().map_err(error_string)?;
    if let Ok(id) = value.parse::<i64>()
        && let Some(project) = projects.iter().find(|project| project.id == id)
    {
        return Ok(project.clone());
    }
    let requested_path = PathBuf::from(value);
    if requested_path.is_absolute()
        && let Ok(project) = registry.get_project(&requested_path)
    {
        return Ok(project);
    }
    let matches = projects
        .into_iter()
        .filter(|project| project.name.eq_ignore_ascii_case(value))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [project] => Ok(project.clone()),
        [] => Err("project was not found by ID, exact path, or exact name".to_owned()),
        _ => Err("project name is ambiguous; use its ID or exact path".to_owned()),
    }
}

fn session_value(session: SessionMetadata) -> Value {
    json!({
        "id": session.id,
        "sourceKind": session.source_kind,
        "sourceThreadId": session.source_thread_id,
        "projectId": session.project_id,
        "projectLinkKind": session.project_link_kind,
        "sourceCwd": session.source_cwd,
        "sourceUpdatedAt": session.source_updated_at,
        "lastSeenAt": session.last_seen_at,
        "sourceLabel": session.source_label,
        "observedStatusLabel": session.observed_status_label,
        "ephemeral": session.ephemeral,
        "textImported": session.text_imported,
        "contentRedacted": true
    })
}

async fn invoke_cli_json(args: &[&str], stdin: Option<&[u8]>) -> Result<Value, String> {
    let executable = std::env::current_exe().map_err(error_string)?;
    invoke_process_json(&executable, args, stdin, CHILD_TIMEOUT).await
}

async fn invoke_process_json(
    executable: &Path,
    args: &[&str],
    body: Option<&[u8]>,
    deadline: Duration,
) -> Result<Value, String> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(if body.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(error_string)?;
    let mut child_stdin = child.stdin.take();
    let stdout = child.stdout.take().ok_or("child stdout was unavailable")?;
    let stderr = child.stderr.take().ok_or("child stderr was unavailable")?;
    let operation = async {
        if let Some(body) = body {
            let mut stdin = child_stdin.take().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "child stdin was unavailable",
                )
            })?;
            stdin.write_all(body).await?;
            stdin.shutdown().await?;
        }
        drop(child_stdin);
        let (stdout, stderr, status) = tokio::try_join!(
            read_bounded(stdout, MAX_CHILD_OUTPUT_BYTES),
            read_bounded(stderr, MAX_CHILD_OUTPUT_BYTES),
            child.wait()
        )?;
        Ok::<_, std::io::Error>((stdout, stderr, status))
    };
    let (stdout, stderr, status) = match timeout(deadline, operation).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            kill_and_reap(&mut child).await;
            return Err(error_string(error));
        }
        Err(_) => {
            kill_and_reap(&mut child).await;
            return Err(format!(
                "child process exceeded the {} second deadline",
                deadline.as_secs_f64()
            ));
        }
    };
    if stdout.len() > MAX_CHILD_OUTPUT_BYTES || stderr.len() > MAX_CHILD_OUTPUT_BYTES {
        return Err("child output exceeded 1 MiB".to_owned());
    }
    if let Ok(value) = serde_json::from_slice(&stdout) {
        return Ok(value);
    }
    if !status.success() {
        let message = sanitize_child_error(&String::from_utf8_lossy(&stderr));
        return Err(if message.is_empty() {
            format!("child exited unsuccessfully: {status}")
        } else {
            message
        });
    }
    Err("child returned malformed JSON".to_owned())
}

async fn read_bounded(reader: impl AsyncRead + Unpin, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    reader
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .await?;
    Ok(bytes)
}

async fn kill_and_reap(child: &mut tokio::process::Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.start_kill();
    }
    let _ = timeout(Duration::from_secs(5), child.wait()).await;
}

fn sanitize_child_error(value: &str) -> String {
    let one_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(512).collect()
}

fn validate_query(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err("text must not be empty".to_owned());
    }
    if value.len() > MAX_TEXT_BYTES {
        return Err("text exceeds 64 KiB".to_owned());
    }
    Ok(())
}

fn validated_request_id(value: Option<String>) -> Result<String, String> {
    let value = value.unwrap_or_else(|| Uuid::new_v4().to_string());
    Uuid::parse_str(&value).map_err(|_| "request_id must be a UUID".to_owned())?;
    Ok(value)
}

fn validate_list_limit(limit: usize) -> Result<usize, String> {
    if !(1..=MAX_LIST_LIMIT).contains(&limit) {
        return Err(format!("limit must be in 1..={MAX_LIST_LIMIT}"));
    }
    Ok(limit)
}

fn validate_pagination_bounds(max_pages: u32, max_threads: usize) -> Result<(), String> {
    if !(1..=100).contains(&max_pages) {
        return Err("max_pages must be in 1..=100".to_owned());
    }
    if !(1..=10_000).contains(&max_threads) {
        return Err("max_threads must be in 1..=10000".to_owned());
    }
    Ok(())
}

fn open_run(state: ControlRunState) -> bool {
    matches!(
        state,
        ControlRunState::Planned
            | ControlRunState::Starting
            | ControlRunState::Active
            | ControlRunState::RecoveryRequired
    )
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn error_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

const fn default_match_limit() -> usize {
    10
}

const fn default_activity_limit() -> usize {
    50
}

fn default_activity_hours() -> f64 {
    24.0
}

const fn default_sync_limit() -> usize {
    100
}

const fn default_since_days() -> usize {
    7
}

const fn default_true() -> bool {
    true
}

const fn default_max_pages() -> u32 {
    100
}

const fn default_max_threads() -> usize {
    10_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn executable_script(body: &str) -> tempfile::TempPath {
        use std::os::unix::fs::PermissionsExt;

        let file = tempfile::NamedTempFile::new().expect("temporary child script");
        let path = file.into_temp_path();
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write child script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("make child script executable");
        path
    }

    #[test]
    fn catalog_preserves_legacy_recall_names_and_marks_mutations() {
        let tools = tool_catalog(true);
        for name in [
            "search_projects",
            "get_project",
            "suggest_codex_command",
            "refresh_index",
            "list_scan_roots",
            "add_scan_root",
            "remove_scan_root",
            "get_context_settings",
            "set_codex_memory_indexing",
            "set_codex_session_indexing",
            "refresh_activity",
            "get_activity_status",
            "get_recent_activity",
        ] {
            assert!(tools.iter().any(|tool| tool.name == name));
        }
        assert!(
            tools
                .iter()
                .find(|tool| tool.name == "search_projects")
                .and_then(|tool| tool.annotations.as_ref())
                .and_then(|annotations| annotations.read_only_hint)
                .unwrap_or(false)
        );
        assert_eq!(
            tools
                .iter()
                .find(|tool| tool.name == "conduct")
                .and_then(|tool| tool.annotations.as_ref())
                .and_then(|annotations| annotations.read_only_hint),
            Some(false)
        );
        let Some(get_run) = tools.iter().find(|tool| tool.name == "get_run") else {
            panic!("get_run tool missing")
        };
        assert_eq!(get_run.input_schema["required"], json!(["run_id"]));
        let Some(reconcile) = tools.iter().find(|tool| tool.name == "reconcile_run") else {
            panic!("reconcile_run tool missing")
        };
        assert_eq!(
            reconcile.input_schema["required"],
            json!(["run_id", "request_id"])
        );
    }

    #[test]
    fn worker_control_tools_require_the_server_startup_capability() {
        let read_only = tool_catalog(false);
        assert!(
            read_only
                .iter()
                .all(|tool| !control_tool(tool.name.as_ref()))
        );
        let enabled = tool_catalog(true);
        for name in ["conduct", "steer_run", "interrupt_run", "reconcile_run"] {
            assert!(enabled.iter().any(|tool| tool.name.as_ref() == name));
        }
    }

    #[test]
    fn context_recall_is_disabled_by_default() {
        let temp = tempfile::tempdir().expect("temporary context state");
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        Registry::open(&paths).expect("initialize context state");
        let value = context_settings(&paths).expect("context settings");
        assert_eq!(value["rawTranscriptIndexing"], false);
        assert_eq!(value["generatedMemoryIndexing"], false);
        assert_eq!(value["deepRecallDefault"], false);
    }

    #[test]
    fn recall_diagnostics_require_explicit_deep_search_and_do_not_expose_content() {
        let temp = tempfile::tempdir().expect("temporary recall state");
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let registry = Registry::open(&paths).expect("initialize recall state");
        registry
            .set_recall_settings(skein_core::RecallSettings {
                include_codex_memories: true,
                include_codex_sessions: false,
            })
            .expect("enable synthetic memory gate without reading sources");

        let quick = search_projects(
            &paths,
            SearchArgs {
                query: "synthetic query".to_owned(),
                limit: 5,
                include_session_text: false,
                include_deep_context: false,
            },
        )
        .expect("quick recall diagnostics");
        assert_eq!(quick["recall"]["mode"], "quick_indexed");
        assert_eq!(quick["recall"]["privateContextAuthorized"], true);
        assert_eq!(quick["recall"]["escalationSuggested"], true);
        assert_eq!(quick["contextMatches"], json!([]));
        assert!(quick["recall"]["contextFreshness"].is_object());

        let deep = search_projects(
            &paths,
            SearchArgs {
                query: "synthetic query".to_owned(),
                limit: 5,
                include_session_text: false,
                include_deep_context: true,
            },
        )
        .expect("explicit deep recall diagnostics");
        assert_eq!(deep["recall"]["mode"], "deep_private");
        assert_eq!(deep["recall"]["escalationSuggested"], false);
        assert_eq!(deep["contextMatches"], json!([]));
        assert_eq!(
            deep["recall"]["sourcesConsulted"],
            json!(["project_metadata", "project_documents", "codex_memory"])
        );
        assert!(
            deep["recall"]["sourcesConsulted"]
                .as_array()
                .is_some_and(|sources| !sources.iter().any(|source| source == "codex_session"))
        );
    }

    #[test]
    fn full_size_text_fits_inside_the_larger_argument_envelope() {
        let arguments = json!({
            "prompt": "x".repeat(MAX_TEXT_BYTES),
            "full_access_acknowledged": true,
            "request_id": "00000000-0000-4000-8000-000000000001"
        });
        assert!(serde_json::to_vec(&arguments).expect("arguments").len() > MAX_TEXT_BYTES);
        assert!(
            serde_json::to_vec(&arguments).expect("arguments").len() <= MAX_TOOL_ARGUMENT_BYTES
        );
        assert!(validate_query(arguments["prompt"].as_str().expect("prompt")).is_ok());
    }

    #[test]
    fn shell_command_quotes_paths_without_execution() {
        assert_eq!(
            shell_quote(Path::new("/synthetic/it's here")),
            "'/synthetic/it'\\''s here'"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn child_receipt_wins_over_a_nonzero_exit() {
        let script = executable_script(
            "printf '%s' '{\"ok\":true,\"durable\":true}'\nprintf '%s' 'synthetic failure' >&2\nexit 7",
        );
        let script_arg = script.to_string_lossy();
        let value = invoke_process_json(
            Path::new("/bin/sh"),
            &[script_arg.as_ref()],
            None,
            Duration::from_secs(2),
        )
        .await
        .expect("structured receipt");
        assert_eq!(value["ok"], true);
        assert_eq!(value["durable"], true);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn child_output_is_acquired_concurrently_and_bounded() {
        let script = executable_script(
            "dd if=/dev/zero bs=1048577 count=1 2>/dev/null || true\n\
             dd if=/dev/zero bs=1048577 count=1 2>/dev/null >&2 || true\n\
             exit 1",
        );
        let script_arg = script.to_string_lossy();
        let error = invoke_process_json(
            Path::new("/bin/sh"),
            &[script_arg.as_ref()],
            None,
            Duration::from_secs(3),
        )
        .await
        .expect_err("oversized child output");
        assert_eq!(error, "child output exceeded 1 MiB");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn child_deadline_kills_and_reaps_the_process() {
        let script = executable_script("exec sleep 30");
        let script_arg = script.to_string_lossy();
        let started = std::time::Instant::now();
        let error = invoke_process_json(
            Path::new("/bin/sh"),
            &[script_arg.as_ref()],
            None,
            Duration::from_millis(50),
        )
        .await
        .expect_err("child timeout");
        assert!(error.contains("deadline"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_the_tool_future_kills_its_child() {
        let script = executable_script("printf '%s' \"$$\" > \"$1\"\nexec sleep 30");
        let pid_file = tempfile::NamedTempFile::new().expect("pid file");
        let pid_path = pid_file.path().to_path_buf();
        let script_path = script.to_string_lossy().into_owned();
        let child_pid_path = pid_path.to_string_lossy().into_owned();
        let task = tokio::spawn(async move {
            invoke_process_json(
                Path::new("/bin/sh"),
                &[script_path.as_str(), child_pid_path.as_str()],
                None,
                Duration::from_secs(30),
            )
            .await
        });
        let pid = timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(value) = std::fs::read_to_string(&pid_path)
                    && !value.is_empty()
                {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("child pid");
        task.abort();
        let _ = task.await;
        let stopped = timeout(Duration::from_secs(2), async {
            loop {
                if !std::process::Command::new("kill")
                    .args(["-0", pid.as_str()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(stopped.is_ok(), "cancelled child {pid} remained alive");
    }
}
