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
    let mut registry = Registry::open(paths)?;

    // Resolve the entire selection before discovery. Invalid selectors therefore
    // cannot cause even a partial traversal or index replacement.
    let (scope_value, discovery, projects, scoped) = match scope {
        IndexScope::All => {
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

    let refreshed = refresh_git(&registry, &projects, working_tree, force);
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
        let context = serde_json::to_value(registry.refresh_context_documents(
            &crate::codex_home(None)?,
            skein_core::ContextDocumentRefreshOptions::default(),
        )?)?;
        let sessions = match crate::sync_codex_catalog_default(paths) {
            Ok(report) => json!({"ok": true, "report": report}),
            Err(error) => json!({"ok": false, "error": error.to_string()}),
        };
        (context, sessions)
    };

    Ok(IndexReport {
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
