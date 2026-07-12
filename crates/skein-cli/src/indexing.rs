use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Value, json};
use skein_core::{DiscoveryReport, Project, Registry, SkeinPaths};

#[derive(Clone, Debug)]
pub(crate) enum IndexScope {
    All,
    Project(PathBuf),
    ScanRoot(PathBuf),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IndexReport {
    started_at: i64,
    completed_at: i64,
    may_be_stale: bool,
    scope: Value,
    discovery: Vec<DiscoveryReport>,
    refreshed: Vec<Value>,
    reports: Vec<Value>,
    documents: Vec<Value>,
    context: Value,
    sessions: Value,
    deferred: Vec<Value>,
}

pub(crate) fn refresh_index(
    paths: &SkeinPaths,
    scope: IndexScope,
    working_tree: bool,
    force: bool,
) -> Result<IndexReport, Box<dyn std::error::Error>> {
    refresh_index_with_progress(paths, scope, working_tree, force, |_| {})
}

pub(crate) fn refresh_index_with_progress(
    paths: &SkeinPaths,
    scope: IndexScope,
    working_tree: bool,
    force: bool,
    mut progress: impl FnMut(&str),
) -> Result<IndexReport, Box<dyn std::error::Error>> {
    progress("validating index scope");
    let started_at = crate::unix_timestamp();
    let mut registry = Registry::open(paths)?;

    // Resolve the entire selection before discovery. Invalid selectors therefore
    // cannot cause even a partial traversal or index replacement.
    let (scope_value, discovery, projects, scoped) = match scope {
        IndexScope::All => {
            progress("discovering configured scan roots");
            let discovery = registry.discover_all_scan_roots()?;
            let projects = registry.list_projects()?;
            (json!({"kind": "all"}), discovery, projects, false)
        }
        IndexScope::Project(path) => {
            let project = registry.get_project(&path)?;
            (
                json!({"kind": "project", "path": project.path}),
                Vec::new(),
                vec![project],
                true,
            )
        }
        IndexScope::ScanRoot(path) => {
            // This non-traversing lookup validates registration first and supplies
            // the retained cache if discovery finds the root offline.
            progress("discovering selected scan root");
            let cached = registry.projects_for_scan_root(&path)?;
            let report = registry.discover_scan_root(&path)?;
            let projects = if report.unreachable {
                cached
            } else {
                registry.projects_for_scan_root(&report.root.path)?
            };
            (
                json!({"kind": "scan_root", "path": report.root.path}),
                vec![report],
                projects,
                true,
            )
        }
    };

    progress("refreshing bounded Git metadata");
    let refreshed = refresh_git(&registry, &projects, working_tree, force);
    progress("refreshing project identity documents");
    let documents = refresh_documents(&mut registry, &projects);
    let mut deferred = discovery
        .iter()
        .filter(|report| report.unreachable)
        .map(|report| {
            json!({
                "source": "scan_root",
                "path": report.root.path,
                "reason": "offline",
                "cachedProjectsRetained": true
            })
        })
        .collect::<Vec<_>>();

    let (context, sessions) = if scoped {
        deferred.extend([
            json!({
                "source": "context",
                "reason": "global_source_excluded_by_scope"
            }),
            json!({
                "source": "sessions",
                "reason": "global_source_excluded_by_scope"
            }),
        ]);
        (
            json!({"status": "deferred", "reason": "global_source_excluded_by_scope"}),
            json!({"status": "deferred", "reason": "global_source_excluded_by_scope"}),
        )
    } else {
        progress("refreshing enabled private context sources");
        let context = serde_json::to_value(registry.refresh_context_documents(
            &crate::codex_home(None)?,
            skein_core::ContextDocumentRefreshOptions::default(),
        )?)?;
        progress("synchronizing bounded Codex session metadata");
        let sessions = match crate::sync_codex_catalog_default(paths) {
            Ok(report) => json!({"ok": true, "report": report}),
            Err(error) => json!({"ok": false, "error": error.to_string()}),
        };
        (context, sessions)
    };

    let may_be_stale = !deferred.is_empty()
        || refreshed.iter().any(|report| report["ok"] == false)
        || documents.iter().any(|report| report["ok"] == false)
        || sessions["ok"] == false
        || ["memories", "sessions"].iter().any(|source| {
            context[*source]["status"]
                .as_str()
                .is_some_and(|status| status.starts_with("deferred_"))
        });
    progress("index refresh complete");
    Ok(IndexReport {
        started_at,
        completed_at: crate::unix_timestamp(),
        may_be_stale,
        scope: scope_value,
        discovery,
        reports: refreshed.clone(),
        refreshed,
        documents,
        context,
        sessions,
        deferred,
    })
}

fn refresh_git(
    registry: &Registry,
    projects: &[Project],
    working_tree: bool,
    force: bool,
) -> Vec<Value> {
    projects
        .iter()
        .map(
            |project| match registry.refresh_project(&project.path, working_tree, force) {
                Ok(report) => crate::value_with_ok(report),
                Err(error) => {
                    json!({"ok": false, "projectPath": project.path, "error": error.to_string()})
                }
            },
        )
        .collect()
}

fn refresh_documents(registry: &mut Registry, projects: &[Project]) -> Vec<Value> {
    projects
        .iter()
        .map(
            |project| match registry.refresh_project_documents(&project.path) {
                Ok(report) => crate::value_with_ok(report),
                Err(error) => {
                    json!({"ok": false, "projectPath": project.path, "error": error.to_string()})
                }
            },
        )
        .collect()
}
