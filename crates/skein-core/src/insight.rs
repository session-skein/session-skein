//! Deterministic, content-bounded matching and activity summaries.

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;

use crate::ControlRun;
use crate::ControlRunState;
use crate::Project;
use crate::ProjectLinkKind;
use crate::Registry;
use crate::Result;
use crate::session::SessionMetadata;

/// Options for one ephemeral metadata match.
pub struct MatchOptions<'a> {
    pub query: &'a str,
    pub include_text: bool,
    pub limit: usize,
    pub now: i64,
}

struct MatchTerms<'a> {
    lower: &'a str,
    tokens: &'a BTreeSet<String>,
}

/// Content-free explanation for one scoring contribution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchEvidence {
    pub family: String,
    pub kind: String,
    pub points: i32,
    pub matches: usize,
}

/// Suggested existing session inside a matched project.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMatch {
    pub source_kind: String,
    pub source_thread_id: String,
    pub project_link_kind: ProjectLinkKind,
    pub source_updated_at: i64,
    pub resumable: bool,
    pub score: i32,
    pub evidence: Vec<MatchEvidence>,
}

/// Bounded project identity exposed by matching without Git subject text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MatchedProject {
    pub id: i64,
    pub name: String,
    pub path: std::path::PathBuf,
}

/// One registered project candidate with auditable integer evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMatch {
    pub project: MatchedProject,
    pub score: i32,
    pub latest_matched_at: Option<i64>,
    pub suggested_session: Option<SessionMatch>,
    pub evidence: Vec<MatchEvidence>,
}

/// Deterministic confidence label; it is not a probability.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchConfidence {
    High,
    Medium,
    Low,
    None,
}

/// Non-dispatching recommendation from one private stdin query.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchRecommendation {
    pub project_id: i64,
    pub source_thread_id: Option<String>,
    pub action: String,
    pub confidence: MatchConfidence,
    pub ambiguous: bool,
    pub score: i32,
    pub runner_up_margin: i32,
    pub dispatchable: bool,
}

/// Complete ephemeral match report. Query text and tokens are never included.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchReport {
    pub schema_version: u32,
    pub query_bytes: usize,
    pub query_token_count: usize,
    pub as_of: i64,
    pub recommendation: Option<MatchRecommendation>,
    pub candidates: Vec<ProjectMatch>,
    pub content_persisted: bool,
}

/// Aggregate facts used by a deterministic project card.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCardFacts {
    pub linked_sessions: usize,
    pub latest_session_at: Option<i64>,
    pub control_runs: usize,
    pub active_runs: usize,
    pub completed_runs: usize,
    pub failed_runs: usize,
    pub interrupted_runs: usize,
    pub recovery_runs: usize,
    pub latest_control_at: Option<i64>,
}

/// Read-only project-library description derived from current durable metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCard {
    pub project: Project,
    pub title: String,
    pub narrative: String,
    pub facts: ProjectCardFacts,
    pub last_activity_at: Option<i64>,
    pub generated: bool,
    pub persisted: bool,
}

/// Honest coverage boundary for a metadata-only activity digest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryCoverage {
    pub codex_sessions: String,
    pub controlled_runs: String,
    pub git: String,
    pub working_tree: String,
    pub untracked_files: bool,
    pub external_shell_work: bool,
}

/// One project's activity within a caller-provided calendar boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayProjectActivity {
    pub project_id: i64,
    pub project_name: String,
    pub session_observations: usize,
    pub source_threads_updated: usize,
    pub runs_created: usize,
    pub runs_completed: usize,
    pub runs_failed: usize,
    pub runs_interrupted: usize,
    pub recovery_runs: usize,
    pub latest_record_at: i64,
    pub git_snapshot_observed: bool,
    pub latest_git_commit_in_window: bool,
}

/// Read-only daily metadata digest. The CLI supplies local-time boundaries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaySummary {
    pub date: String,
    pub timezone: String,
    pub start_at: i64,
    pub end_at: i64,
    pub narrative: String,
    pub projects: Vec<DayProjectActivity>,
    pub coverage: SummaryCoverage,
    pub persisted: bool,
}

impl Registry {
    /// Rank registered projects and their strongest linked session without writing state.
    pub fn match_metadata(&self, options: &MatchOptions<'_>) -> Result<MatchReport> {
        let query_tokens = tokens(options.query);
        let query_lower = options.query.to_lowercase();
        let terms = MatchTerms {
            lower: &query_lower,
            tokens: &query_tokens,
        };
        let projects = self.list_projects()?;
        let exact_path_project_ids = longest_exact_path_matches(&projects, &query_lower);
        let sessions = self.list_session_metadata()?;
        let runs = self.list_control_runs()?;
        let sessions_by_project = group_sessions(&sessions);
        let runs_by_project = group_runs(&runs);
        let text = if options.include_text {
            self.list_session_match_text()?
                .into_iter()
                .map(|item| (item.id, (item.name, item.preview)))
                .collect::<HashMap<_, _>>()
        } else {
            HashMap::new()
        };
        let mut candidates = projects
            .into_iter()
            .filter_map(|project| {
                let project_id = project.id;
                build_candidate(
                    project,
                    sessions_by_project
                        .get(&project_id)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                    runs_by_project
                        .get(&project_id)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                    &text,
                    &terms,
                    exact_path_project_ids.contains(&project_id),
                    options,
                )
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| right.latest_matched_at.cmp(&left.latest_matched_at))
                .then_with(|| {
                    left.project
                        .name
                        .to_lowercase()
                        .cmp(&right.project.name.to_lowercase())
                })
                .then_with(|| left.project.path.cmp(&right.project.path))
                .then_with(|| left.project.id.cmp(&right.project.id))
        });
        let recommendation = recommendation(&candidates);
        candidates.truncate(options.limit);
        Ok(MatchReport {
            schema_version: 1,
            query_bytes: options.query.len(),
            query_token_count: query_tokens.len(),
            as_of: options.now,
            recommendation,
            candidates,
            content_persisted: false,
        })
    }

    /// Render all project cards from already observed metadata only.
    pub fn project_cards(&self) -> Result<Vec<ProjectCard>> {
        let sessions = self.list_session_metadata()?;
        let runs = self.list_control_runs()?;
        let sessions_by_project = group_sessions(&sessions);
        let runs_by_project = group_runs(&runs);
        self.list_projects()?
            .into_iter()
            .map(|project| {
                let project_id = project.id;
                build_card(
                    project,
                    sessions_by_project
                        .get(&project_id)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                    runs_by_project
                        .get(&project_id)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                )
            })
            .collect()
    }

    /// Render one project card without touching Git or Codex.
    pub fn project_card(&self, path: &Path) -> Result<ProjectCard> {
        let project = self.get_project(path)?;
        let sessions = self.list_session_metadata()?;
        let runs = self.list_control_runs()?;
        let project_sessions = sessions
            .iter()
            .filter(|session| session.project_id == Some(project.id))
            .collect::<Vec<_>>();
        let project_runs = runs
            .iter()
            .filter(|run| run.project_id == project.id)
            .collect::<Vec<_>>();
        build_card(project, &project_sessions, &project_runs)
    }

    /// Summarize activity whose durable timestamps fall inside `[start_at, end_at)`.
    pub fn day_summary(
        &self,
        date: &str,
        timezone: &str,
        start_at: i64,
        end_at: i64,
    ) -> Result<DaySummary> {
        let projects = self.list_projects()?;
        let sessions = self.list_session_metadata()?;
        let runs = self.list_control_runs()?;
        let sessions_by_project = group_sessions(&sessions);
        let runs_by_project = group_runs(&runs);
        let mut activities = Vec::new();
        for project in projects {
            let project_sessions = sessions_by_project
                .get(&project.id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let project_runs = runs_by_project
                .get(&project.id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let session_observations = project_sessions
                .iter()
                .filter(|session| in_window(session.last_seen_at, start_at, end_at))
                .count();
            let source_threads_updated = project_sessions
                .iter()
                .filter(|session| in_window(session.source_updated_at, start_at, end_at))
                .count();
            let runs_created = project_runs
                .iter()
                .filter(|run| in_window(run.created_at, start_at, end_at))
                .count();
            let terminal = |state| {
                project_runs
                    .iter()
                    .filter(|run| run.state == state)
                    .filter(|run| {
                        run.terminal_at
                            .is_some_and(|at| in_window(at, start_at, end_at))
                    })
                    .count()
            };
            let recovery_runs = project_runs
                .iter()
                .filter(|run| run.state == ControlRunState::RecoveryRequired)
                .filter(|run| in_window(run.updated_at, start_at, end_at))
                .count();
            let git_snapshot_observed = project
                .metadata_refreshed_at
                .is_some_and(|at| in_window(at, start_at, end_at));
            let latest_git_commit_in_window = project
                .git
                .as_ref()
                .and_then(|git| git.last_commit_at)
                .is_some_and(|at| in_window(at, start_at, end_at));
            let latest_record_at = project_sessions
                .iter()
                .map(|session| session.last_seen_at)
                .filter(|at| in_window(*at, start_at, end_at))
                .chain(
                    project_sessions
                        .iter()
                        .map(|session| session.source_updated_at)
                        .filter(|at| in_window(*at, start_at, end_at)),
                )
                .chain(
                    project_runs
                        .iter()
                        .map(|run| run.created_at)
                        .filter(|at| in_window(*at, start_at, end_at)),
                )
                .chain(
                    project_runs
                        .iter()
                        .filter_map(|run| run.terminal_at)
                        .filter(|at| in_window(*at, start_at, end_at)),
                )
                .chain(
                    project_runs
                        .iter()
                        .filter(|run| run.state == ControlRunState::RecoveryRequired)
                        .map(|run| run.updated_at)
                        .filter(|at| in_window(*at, start_at, end_at)),
                )
                .chain(
                    project
                        .metadata_refreshed_at
                        .filter(|at| in_window(*at, start_at, end_at)),
                )
                .chain(
                    project
                        .git
                        .as_ref()
                        .and_then(|git| git.last_commit_at)
                        .filter(|at| in_window(*at, start_at, end_at)),
                )
                .max();
            let Some(latest_record_at) = latest_record_at else {
                continue;
            };
            activities.push(DayProjectActivity {
                project_id: project.id,
                project_name: project.name,
                session_observations,
                source_threads_updated,
                runs_created,
                runs_completed: terminal(ControlRunState::Completed),
                runs_failed: terminal(ControlRunState::Failed),
                runs_interrupted: terminal(ControlRunState::Interrupted),
                recovery_runs,
                latest_record_at,
                git_snapshot_observed,
                latest_git_commit_in_window,
            });
        }
        activities.sort_by(|left, right| {
            right
                .latest_record_at
                .cmp(&left.latest_record_at)
                .then_with(|| {
                    left.project_name
                        .to_lowercase()
                        .cmp(&right.project_name.to_lowercase())
                })
                .then_with(|| left.project_id.cmp(&right.project_id))
        });
        let sessions = activities
            .iter()
            .map(|item| item.session_observations)
            .sum::<usize>();
        let completed = activities
            .iter()
            .map(|item| item.runs_completed)
            .sum::<usize>();
        let recovery = activities
            .iter()
            .map(|item| item.recovery_runs)
            .sum::<usize>();
        let narrative = if activities.is_empty() {
            format!("For {date}, current metadata has no bounded project activity record.")
        } else {
            format!(
                "For {date}, current metadata has dated records in {} registered project(s): {sessions} session metadata observation(s), {completed} completed controlled run(s), and {recovery} currently recovery-required run(s).",
                activities.len()
            )
        };
        Ok(DaySummary {
            date: date.to_owned(),
            timezone: timezone.to_owned(),
            start_at,
            end_at,
            narrative,
            projects: activities,
            coverage: SummaryCoverage {
                codex_sessions: "mutable latest-observation snapshot, not an event log".to_owned(),
                controlled_runs: "current run rows and terminal timestamps; intermediate states are not reconstructed".to_owned(),
                git: "current stored snapshot; latest commit source time is separate from refresh observation time".to_owned(),
                working_tree: "only explicit tracked-working-tree refreshes".to_owned(),
                untracked_files: false,
                external_shell_work: false,
            },
            persisted: false,
        })
    }
}

fn group_sessions(sessions: &[SessionMetadata]) -> HashMap<i64, Vec<&SessionMetadata>> {
    let mut grouped = HashMap::<i64, Vec<&SessionMetadata>>::new();
    for session in sessions {
        if let Some(project_id) = session.project_id {
            grouped.entry(project_id).or_default().push(session);
        }
    }
    grouped
}

fn group_runs(runs: &[ControlRun]) -> HashMap<i64, Vec<&ControlRun>> {
    let mut grouped = HashMap::<i64, Vec<&ControlRun>>::new();
    for run in runs {
        grouped.entry(run.project_id).or_default().push(run);
    }
    grouped
}

fn build_candidate(
    project: Project,
    sessions: &[&SessionMetadata],
    runs: &[&ControlRun],
    text: &HashMap<i64, (Option<String>, Option<String>)>,
    terms: &MatchTerms<'_>,
    exact_path: bool,
    options: &MatchOptions<'_>,
) -> Option<ProjectMatch> {
    let mut evidence = Vec::new();
    let mut latest = None;
    if exact_path {
        push_evidence(&mut evidence, "project_path", "exact_path", 50, 1);
    }
    let name_tokens = tokens(&project.name);
    if !name_tokens.is_empty()
        && ((name_tokens.len() >= 2 && terms.tokens.is_superset(&name_tokens))
            || options.query.trim().eq_ignore_ascii_case(&project.name))
    {
        push_evidence(&mut evidence, "project_identity", "exact_name", 50, 1);
    }
    add_token_evidence(
        &mut evidence,
        "project_identity",
        "name_tokens",
        terms.tokens,
        &name_tokens,
        12,
        3,
    );
    add_token_evidence(
        &mut evidence,
        "project_path",
        "basename_tokens",
        terms.tokens,
        &tokens(
            project
                .path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default(),
        ),
        8,
        3,
    );
    if let Some(git) = &project.git {
        if let Some(branch) = &git.head_ref {
            add_token_evidence(
                &mut evidence,
                "git",
                "branch_tokens",
                terms.tokens,
                &tokens(branch),
                6,
                2,
            );
        }
        if let Some(subject) = &git.last_commit_subject {
            add_token_evidence(
                &mut evidence,
                "git",
                "commit_subject_tokens",
                terms.tokens,
                &tokens(subject),
                4,
                4,
            );
        }
    }
    let project_lexical = !evidence.is_empty();
    let mut session_matches = sessions
        .iter()
        .filter_map(|session| {
            build_session_match(
                session,
                text.get(&session.id),
                terms.lower,
                terms.tokens,
                options,
                project_lexical,
            )
        })
        .collect::<Vec<_>>();
    session_matches.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| right.source_updated_at.cmp(&left.source_updated_at))
            .then_with(|| left.source_kind.cmp(&right.source_kind))
            .then_with(|| left.source_thread_id.cmp(&right.source_thread_id))
    });
    let suggested_session = session_matches.into_iter().next();
    if let Some(session) = &suggested_session {
        latest = Some(session.source_updated_at);
    }
    let lexical = project_lexical
        || suggested_session.as_ref().is_some_and(|session| {
            session.evidence.iter().any(|item| {
                matches!(
                    item.kind.as_str(),
                    "exact_thread" | "cwd_basename_tokens" | "name_tokens" | "preview_tokens"
                )
            })
        });
    if !lexical {
        return None;
    }
    let recovery = runs
        .iter()
        .filter(|run| run.state == ControlRunState::RecoveryRequired)
        .count();
    if recovery > 0 {
        push_evidence(&mut evidence, "control", "recovery_run", 12, recovery);
    }
    latest = latest.max(runs.iter().map(|run| run.updated_at).max());
    let score = evidence.iter().map(|item| item.points).sum::<i32>()
        + suggested_session
            .as_ref()
            .map_or(0, |session| session.score);
    Some(ProjectMatch {
        project: MatchedProject {
            id: project.id,
            name: project.name,
            path: project.path,
        },
        score,
        latest_matched_at: latest,
        suggested_session,
        evidence,
    })
}

fn longest_exact_path_matches(projects: &[Project], query_lower: &str) -> HashSet<i64> {
    let mut matches = projects
        .iter()
        .filter_map(|project| {
            let path = project.path.to_string_lossy().to_lowercase();
            bounded_path_occurs(query_lower, &path).then_some((project.id, path.len()))
        })
        .collect::<Vec<_>>();
    let Some(longest) = matches.iter().map(|(_, length)| *length).max() else {
        return HashSet::new();
    };
    matches.retain(|(_, length)| *length == longest);
    matches.into_iter().map(|(id, _)| id).collect()
}

fn bounded_path_occurs(query: &str, path: &str) -> bool {
    query.match_indices(path).any(|(start, matched)| {
        let before = query[..start].chars().next_back();
        let after = query[start + matched.len()..].chars().next();
        before.is_none_or(|character| !is_path_character(character))
            && after.is_none_or(|character| !is_path_character(character))
    })
}

fn is_path_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '/' | '\\' | '.' | '_' | '-')
}

fn build_session_match(
    session: &SessionMetadata,
    text: Option<&(Option<String>, Option<String>)>,
    query_lower: &str,
    query_tokens: &BTreeSet<String>,
    options: &MatchOptions<'_>,
    project_lexical: bool,
) -> Option<SessionMatch> {
    let mut evidence = Vec::new();
    let exact_thread = query_lower.contains(&session.source_thread_id.to_lowercase());
    let generic_eligible = !session.ephemeral
        && session.observed_status_label != "systemError"
        && !session.source_label.starts_with("subAgent")
        && session.project_link_kind != ProjectLinkKind::ManualUnbound;
    if !exact_thread && !generic_eligible {
        return None;
    }
    if exact_thread {
        push_evidence(&mut evidence, "session_identity", "exact_thread", 100, 1);
    }
    add_token_evidence(
        &mut evidence,
        "session_path",
        "cwd_basename_tokens",
        query_tokens,
        &tokens(
            session
                .source_cwd
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default(),
        ),
        8,
        3,
    );
    if options.include_text
        && session.text_imported
        && let Some((name, preview)) = text
    {
        if let Some(name) = name {
            add_token_evidence(
                &mut evidence,
                "session_text",
                "name_tokens",
                query_tokens,
                &tokens(name),
                10,
                3,
            );
        }
        if let Some(preview) = preview {
            add_token_evidence(
                &mut evidence,
                "session_text",
                "preview_tokens",
                query_tokens,
                &tokens(preview),
                4,
                4,
            );
        }
    }
    let session_lexical = !evidence.is_empty();
    if !project_lexical && !session_lexical {
        return None;
    }
    let link_points = match session.project_link_kind {
        ProjectLinkKind::Manual => 6,
        ProjectLinkKind::Automatic => 3,
        ProjectLinkKind::ManualUnbound | ProjectLinkKind::Unmatched => 0,
    };
    if link_points > 0 {
        push_evidence(
            &mut evidence,
            "session_link",
            "project_link",
            link_points,
            1,
        );
    }
    let freshness = recency_points(options.now, session.source_updated_at);
    if freshness > 0 {
        push_evidence(&mut evidence, "session_recency", "updated", freshness, 1);
    }
    let score = evidence.iter().map(|item| item.points).sum();
    Some(SessionMatch {
        source_kind: session.source_kind.clone(),
        source_thread_id: session.source_thread_id.clone(),
        project_link_kind: session.project_link_kind,
        source_updated_at: session.source_updated_at,
        resumable: !session.ephemeral,
        score,
        evidence,
    })
}

fn recommendation(candidates: &[ProjectMatch]) -> Option<MatchRecommendation> {
    let top = candidates.first()?;
    let margin = top.score - candidates.get(1).map_or(0, |candidate| candidate.score);
    let families = top
        .evidence
        .iter()
        .map(|item| item.family.as_str())
        .chain(
            top.suggested_session
                .iter()
                .flat_map(|session| session.evidence.iter().map(|item| item.family.as_str())),
        )
        .collect::<BTreeSet<_>>();
    let exact_thread = top.suggested_session.as_ref().is_some_and(|session| {
        session
            .evidence
            .iter()
            .any(|item| item.kind == "exact_thread")
    });
    let exact_path = top.evidence.iter().any(|item| item.kind == "exact_path");
    let unique_exact_name =
        margin >= 15 && top.evidence.iter().any(|item| item.kind == "exact_name");
    let unique_exact_thread = exact_thread && margin >= 15;
    let confidence = if unique_exact_thread
        || exact_path
        || unique_exact_name
        || (top.score >= 50 && families.len() >= 2 && margin >= 15)
    {
        MatchConfidence::High
    } else if top.score >= 20 && margin >= 6 {
        MatchConfidence::Medium
    } else if top.score > 0 {
        MatchConfidence::Low
    } else {
        MatchConfidence::None
    };
    let ambiguous = candidates.len() > 1 && margin < 6;
    let continuity = top
        .suggested_session
        .as_ref()
        .is_some_and(|session| unique_exact_thread && session.resumable);
    Some(MatchRecommendation {
        project_id: top.project.id,
        source_thread_id: continuity
            .then(|| {
                top.suggested_session
                    .as_ref()
                    .map(|session| session.source_thread_id.clone())
            })
            .flatten(),
        action: if continuity { "resume" } else { "start" }.to_owned(),
        confidence,
        ambiguous,
        score: top.score,
        runner_up_margin: margin,
        dispatchable: false,
    })
}

fn build_card(
    project: Project,
    sessions: &[&SessionMetadata],
    runs: &[&ControlRun],
) -> Result<ProjectCard> {
    let count = |state| runs.iter().filter(|run| run.state == state).count();
    let latest_session_at = sessions
        .iter()
        .map(|session| session.source_updated_at)
        .max();
    let latest_control_at = runs.iter().map(|run| run.updated_at).max();
    let last_activity_at = latest_session_at
        .max(latest_control_at)
        .max(project.git.as_ref().and_then(|git| git.last_commit_at));
    let facts = ProjectCardFacts {
        linked_sessions: sessions.len(),
        latest_session_at,
        control_runs: runs.len(),
        active_runs: count(ControlRunState::Active),
        completed_runs: count(ControlRunState::Completed),
        failed_runs: count(ControlRunState::Failed),
        interrupted_runs: count(ControlRunState::Interrupted),
        recovery_runs: count(ControlRunState::RecoveryRequired),
        latest_control_at,
    };
    let chapter = project
        .git
        .as_ref()
        .and_then(|git| git.last_commit_subject.as_deref())
        .map(|subject| format!(" Its latest recorded chapter is “{subject}.”"))
        .unwrap_or_default();
    let branch = project
        .git
        .as_ref()
        .and_then(|git| git.head_ref.as_deref())
        .map(|branch| format!(" is currently observed on {branch}."))
        .unwrap_or_else(|| " has no observed Git branch.".to_owned());
    let state = if facts.recovery_runs > 0 {
        format!(
            " {} run(s) are recorded as requiring recovery.",
            facts.recovery_runs
        )
    } else if facts.active_runs > 0 {
        format!(
            " {} controlled run(s) remain recorded in active state; this card does not prove live ownership.",
            facts.active_runs
        )
    } else {
        " No active or recovery-required run is recorded.".to_owned()
    };
    let narrative = format!(
        "{}{}{} {} linked Codex thread(s) and {} controlled run(s) are in the local catalog.{}",
        project.name, branch, chapter, facts.linked_sessions, facts.control_runs, state
    );
    Ok(ProjectCard {
        title: project.name.clone(),
        project,
        narrative,
        facts,
        last_activity_at,
        generated: true,
        persisted: false,
    })
}

fn add_token_evidence(
    evidence: &mut Vec<MatchEvidence>,
    family: &str,
    kind: &str,
    query: &BTreeSet<String>,
    field: &BTreeSet<String>,
    points_each: i32,
    cap: usize,
) {
    let matches = query.intersection(field).count().min(cap);
    if matches > 0 {
        push_evidence(
            evidence,
            family,
            kind,
            points_each * i32::try_from(matches).unwrap_or(i32::MAX),
            matches,
        );
    }
}

fn push_evidence(
    evidence: &mut Vec<MatchEvidence>,
    family: &str,
    kind: &str,
    points: i32,
    matches: usize,
) {
    evidence.push(MatchEvidence {
        family: family.to_owned(),
        kind: kind.to_owned(),
        points,
        matches,
    });
}

fn recency_points(now: i64, observed_at: i64) -> i32 {
    let age = now.saturating_sub(observed_at).max(0);
    match age {
        0..=86_400 => 8,
        86_401..=604_800 => 5,
        604_801..=2_592_000 => 2,
        _ => 0,
    }
}

fn in_window(value: i64, start: i64, end: i64) -> bool {
    value >= start && value < end
}

fn tokens(value: &str) -> BTreeSet<String> {
    let mut words = BTreeSet::new();
    let mut current = String::new();
    let mut previous_lowercase = false;
    let flush = |current: &mut String, words: &mut BTreeSet<String>| {
        if current.chars().count() >= 2 {
            words.insert(current.to_lowercase());
        }
        current.clear();
    };
    for character in value.chars() {
        if character.is_alphanumeric() {
            if character.is_uppercase() && previous_lowercase {
                flush(&mut current, &mut words);
            }
            current.extend(character.to_lowercase());
            previous_lowercase = character.is_lowercase();
        } else {
            flush(&mut current, &mut words);
            previous_lowercase = false;
        }
    }
    flush(&mut current, &mut words);
    words
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use crate::SessionObservation;
    use crate::SkeinPaths;

    use super::*;

    fn fixture() -> Result<(tempfile::TempDir, Registry, PathBuf, PathBuf)> {
        let temp = tempfile::tempdir().map_err(|source| crate::Error::Io {
            path: PathBuf::from("temporary insights fixture"),
            source,
        })?;
        let alpha = temp.path().join("alpha-renderer");
        let beta = temp.path().join("beta-api");
        fs::create_dir(&alpha).map_err(|source| crate::Error::Io {
            path: alpha.clone(),
            source,
        })?;
        fs::create_dir(&beta).map_err(|source| crate::Error::Io {
            path: beta.clone(),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let registry = Registry::open(&paths)?;
        registry.add_project(&alpha, Some("Alpha Renderer"))?;
        registry.add_project(&beta, Some("Beta API"))?;
        Ok((temp, registry, alpha, beta))
    }

    #[test]
    fn exact_thread_and_project_evidence_are_stable_and_additive() -> Result<()> {
        let (_temp, mut registry, alpha, _beta) = fixture()?;
        registry.import_sessions(&[SessionObservation {
            source_kind: "codex".to_owned(),
            source_thread_id: "01900000-0000-7000-8000-000000000001".to_owned(),
            source_session_id: None,
            source_cwd: alpha,
            source_created_at: 100,
            source_updated_at: 950,
            source_label: "cli".to_owned(),
            observed_status_label: "notLoaded".to_owned(),
            model_provider: Some("openai".to_owned()),
            source_version: None,
            parent_source_thread_id: None,
            forked_from_source_thread_id: None,
            ephemeral: false,
            name: None,
            preview: None,
            text_imported: false,
        }])?;
        let report = registry.match_metadata(&MatchOptions {
            query: "continue Alpha Renderer 01900000-0000-7000-8000-000000000001",
            include_text: false,
            limit: 5,
            now: 1_000,
        })?;
        assert_eq!(report.candidates[0].project.name, "Alpha Renderer");
        assert_eq!(
            report.candidates[0].score,
            report.candidates[0]
                .evidence
                .iter()
                .map(|item| item.points)
                .sum::<i32>()
                + report.candidates[0]
                    .suggested_session
                    .as_ref()
                    .map_or(0, |session| session.score)
        );
        let recommendation = report.recommendation.expect("recommendation");
        assert_eq!(recommendation.confidence, MatchConfidence::High);
        assert_eq!(recommendation.action, "resume");
        assert!(!recommendation.dispatchable);
        Ok(())
    }

    #[test]
    fn recency_alone_never_creates_a_candidate() -> Result<()> {
        let (_temp, mut registry, alpha, _beta) = fixture()?;
        registry.import_sessions(&[SessionObservation {
            source_kind: "codex".to_owned(),
            source_thread_id: "recent-thread".to_owned(),
            source_session_id: None,
            source_cwd: alpha,
            source_created_at: 999,
            source_updated_at: 999,
            source_label: "cli".to_owned(),
            observed_status_label: "notLoaded".to_owned(),
            model_provider: None,
            source_version: None,
            parent_source_thread_id: None,
            forked_from_source_thread_id: None,
            ephemeral: false,
            name: None,
            preview: None,
            text_imported: false,
        }])?;
        let report = registry.match_metadata(&MatchOptions {
            query: "unrelated quasar",
            include_text: false,
            limit: 5,
            now: 1_000,
        })?;
        assert!(report.candidates.is_empty());
        assert!(report.recommendation.is_none());
        Ok(())
    }

    #[test]
    fn cards_and_day_summary_never_invent_unobserved_activity() -> Result<()> {
        let (_temp, registry, alpha, _beta) = fixture()?;
        let card = registry.project_card(&alpha)?;
        assert_eq!(card.facts.linked_sessions, 0);
        assert!(card.narrative.contains("0 linked Codex thread"));
        assert!(!card.persisted);
        let day = registry.day_summary("1970-01-01", "UTC", 0, 86_400)?;
        assert!(day.projects.is_empty());
        assert!(day.narrative.contains("no bounded project activity"));
        assert!(!day.persisted);
        Ok(())
    }

    #[test]
    fn presentation_limit_never_changes_tie_confidence() -> Result<()> {
        let (_temp, registry, alpha, beta) = fixture()?;
        registry.add_project(&alpha, Some("Shared Project"))?;
        registry.add_project(&beta, Some("Shared Project"))?;
        let one = registry.match_metadata(&MatchOptions {
            query: "Shared Project",
            include_text: false,
            limit: 1,
            now: 1_000,
        })?;
        let all = registry.match_metadata(&MatchOptions {
            query: "Shared Project",
            include_text: false,
            limit: 5,
            now: 1_000,
        })?;
        assert_eq!(one.candidates.len(), 1);
        assert_eq!(all.candidates.len(), 2);
        assert_eq!(one.recommendation, all.recommendation);
        let recommendation = one.recommendation.expect("recommendation");
        assert!(recommendation.ambiguous);
        assert_eq!(recommendation.confidence, MatchConfidence::Low);
        assert_eq!(recommendation.runner_up_margin, 0);
        Ok(())
    }

    #[test]
    fn short_names_and_shared_path_ancestors_do_not_false_match() -> Result<()> {
        let (_temp, registry, alpha, _beta) = fixture()?;
        registry.add_project(&alpha, Some("AI"))?;
        for query in ["said nothing relevant", "home code labs"] {
            let report = registry.match_metadata(&MatchOptions {
                query,
                include_text: false,
                limit: 5,
                now: 1_000,
            })?;
            assert!(report.candidates.is_empty(), "unexpected match for {query}");
        }
        Ok(())
    }

    #[test]
    fn exact_path_uses_longest_bounded_registered_path() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|source| crate::Error::Io {
            path: PathBuf::from("temporary path matching fixture"),
            source,
        })?;
        let parent = temp.path().join("foo");
        let nested = parent.join("bar");
        let prefix = temp.path().join("api");
        let sibling = temp.path().join("api-v2");
        for path in [&nested, &prefix, &sibling] {
            fs::create_dir_all(path).map_err(|source| crate::Error::Io {
                path: path.clone(),
                source,
            })?;
        }
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let registry = Registry::open(&paths)?;
        registry.add_project(&parent, Some("Foo Parent"))?;
        registry.add_project(&nested, Some("Bar Nested"))?;
        registry.add_project(&prefix, Some("API One"))?;
        registry.add_project(&sibling, Some("API Two"))?;

        for (query_path, expected) in [(&nested, "Bar Nested"), (&sibling, "API Two")] {
            let canonical_query_path = registry.get_project(query_path)?.path;
            let report = registry.match_metadata(&MatchOptions {
                query: &format!("continue {}", canonical_query_path.display()),
                include_text: false,
                limit: 10,
                now: 1_000,
            })?;
            let recommendation = report.recommendation.expect("recommendation");
            let selected = report
                .candidates
                .iter()
                .find(|candidate| candidate.project.id == recommendation.project_id)
                .expect("selected candidate");
            assert_eq!(selected.project.name, expected);
            assert_eq!(recommendation.confidence, MatchConfidence::High);
            assert!(
                selected
                    .evidence
                    .iter()
                    .any(|item| item.kind == "exact_path")
            );
            assert!(
                report
                    .candidates
                    .iter()
                    .filter(|candidate| {
                        candidate
                            .evidence
                            .iter()
                            .any(|item| item.kind == "exact_path")
                    })
                    .count()
                    == 1
            );
        }
        Ok(())
    }

    #[test]
    fn private_session_text_requires_explicit_opt_in_and_is_never_rendered() -> Result<()> {
        let (_temp, mut registry, alpha, _beta) = fixture()?;
        registry.import_sessions(&[SessionObservation {
            source_kind: "codex".to_owned(),
            source_thread_id: "text-thread".to_owned(),
            source_session_id: None,
            source_cwd: alpha,
            source_created_at: 100,
            source_updated_at: 200,
            source_label: "cli".to_owned(),
            observed_status_label: "notLoaded".to_owned(),
            model_provider: None,
            source_version: None,
            parent_source_thread_id: None,
            forked_from_source_thread_id: None,
            ephemeral: false,
            name: Some("ultraviolet sentinel".to_owned()),
            preview: Some("private preview sentinel".to_owned()),
            text_imported: true,
        }])?;
        let default = registry.match_metadata(&MatchOptions {
            query: "ultraviolet",
            include_text: false,
            limit: 5,
            now: 300,
        })?;
        assert!(default.candidates.is_empty());
        let opted_in = registry.match_metadata(&MatchOptions {
            query: "ultraviolet",
            include_text: true,
            limit: 5,
            now: 300,
        })?;
        assert_eq!(opted_in.candidates[0].project.name, "Alpha Renderer");
        let serialized = serde_json::to_string(&opted_in)
            .map_err(|error| crate::Error::InvalidControlRequest(error.to_string()))?;
        assert!(!serialized.contains("ultraviolet"));
        assert!(!serialized.contains("private preview"));
        Ok(())
    }

    #[test]
    fn ephemeral_exact_thread_routes_project_but_never_recommends_resume() -> Result<()> {
        let (_temp, mut registry, alpha, _beta) = fixture()?;
        let thread = "01900000-0000-7000-8000-000000000099";
        registry.import_sessions(&[SessionObservation {
            source_kind: "codex".to_owned(),
            source_thread_id: thread.to_owned(),
            source_session_id: None,
            source_cwd: alpha,
            source_created_at: 100,
            source_updated_at: 200,
            source_label: "cli".to_owned(),
            observed_status_label: "notLoaded".to_owned(),
            model_provider: None,
            source_version: None,
            parent_source_thread_id: None,
            forked_from_source_thread_id: None,
            ephemeral: true,
            name: None,
            preview: None,
            text_imported: false,
        }])?;
        let report = registry.match_metadata(&MatchOptions {
            query: thread,
            include_text: false,
            limit: 5,
            now: 300,
        })?;
        let recommendation = report.recommendation.expect("recommendation");
        assert_eq!(recommendation.confidence, MatchConfidence::High);
        assert_eq!(recommendation.action, "start");
        assert!(recommendation.source_thread_id.is_none());
        assert!(
            !report.candidates[0]
                .suggested_session
                .as_ref()
                .expect("exact session")
                .resumable
        );
        Ok(())
    }

    #[test]
    fn day_summary_separates_observation_time_from_source_time_and_counts_runs() -> Result<()> {
        use crate::NewControlRun;
        use crate::registry::unix_timestamp;

        let (_temp, mut registry, alpha, beta) = fixture()?;
        registry.import_sessions(&[SessionObservation {
            source_kind: "codex".to_owned(),
            source_thread_id: "old-source-thread".to_owned(),
            source_session_id: None,
            source_cwd: alpha.clone(),
            source_created_at: 99,
            source_updated_at: 100,
            source_label: "cli".to_owned(),
            observed_status_label: "notLoaded".to_owned(),
            model_provider: None,
            source_version: None,
            parent_source_thread_id: None,
            forked_from_source_thread_id: None,
            ephemeral: false,
            name: None,
            preview: None,
            text_imported: false,
        }])?;
        let plan = registry.plan_control_run(&NewControlRun {
            project_path: &alpha,
            resume_thread_id: None,
            prompt: "private",
            full_access_acknowledged: true,
        })?;
        registry.begin_control_action(plan.thread_action_id)?;
        registry.acknowledge_thread_action(plan.thread_action_id, "thread", Some("session"))?;
        registry.begin_control_action(plan.turn_action_id)?;
        registry.acknowledge_turn_action(plan.turn_action_id, "turn")?;
        registry.complete_control_run(plan.run_id, "completed")?;
        let recovery = registry.plan_control_run(&NewControlRun {
            project_path: &alpha,
            resume_thread_id: None,
            prompt: "private",
            full_access_acknowledged: true,
        })?;
        registry.begin_control_action(recovery.thread_action_id)?;
        registry.mark_stale_control_runs(true)?;

        let now = unix_timestamp();
        let current = registry.day_summary("current", "UTC", now - 5, now + 5)?;
        let activity = current
            .projects
            .iter()
            .find(|item| item.project_name == "Alpha Renderer")
            .expect("current activity");
        assert_eq!(activity.session_observations, 1);
        assert_eq!(activity.source_threads_updated, 0);
        assert_eq!(activity.runs_created, 2);
        assert_eq!(activity.runs_completed, 1);
        assert_eq!(activity.recovery_runs, 1);

        let before = registry.day_summary("before", "UTC", 99, 100)?;
        assert!(before.projects.is_empty());
        let source_day = registry.day_summary("source", "UTC", 100, 101)?;
        let source = source_day
            .projects
            .iter()
            .find(|item| item.project_name == "Alpha Renderer")
            .expect("source activity");
        assert_eq!(source.session_observations, 0);
        assert_eq!(source.source_threads_updated, 1);

        let cross_day = registry.plan_control_run(&NewControlRun {
            project_path: &beta,
            resume_thread_id: None,
            prompt: "private",
            full_access_acknowledged: true,
        })?;
        registry.begin_control_action(cross_day.thread_action_id)?;
        registry.acknowledge_thread_action(
            cross_day.thread_action_id,
            "cross-day-thread",
            Some("cross-day-session"),
        )?;
        registry.begin_control_action(cross_day.turn_action_id)?;
        registry.acknowledge_turn_action(cross_day.turn_action_id, "cross-day-turn")?;
        registry.complete_control_run(cross_day.run_id, "completed")?;
        registry.connection.execute(
            "UPDATE control_runs SET created_at = 100, updated_at = 200, terminal_at = 200 WHERE id = ?1",
            [cross_day.run_id],
        )?;
        let created_day = registry.day_summary("created", "UTC", 100, 101)?;
        let created = created_day
            .projects
            .iter()
            .find(|item| item.project_name == "Beta API")
            .expect("creation-day activity");
        assert_eq!(created.runs_created, 1);
        assert_eq!(created.runs_completed, 0);
        let terminal_day = registry.day_summary("terminal", "UTC", 200, 201)?;
        let completed = terminal_day
            .projects
            .iter()
            .find(|item| item.project_name == "Beta API")
            .expect("terminal-day activity");
        assert_eq!(completed.runs_created, 0);
        assert_eq!(completed.runs_completed, 1);
        Ok(())
    }

    #[test]
    fn grouping_visits_each_metadata_row_once() {
        let sessions = (0..1_000)
            .map(|id| SessionMetadata {
                id,
                source_kind: "codex".to_owned(),
                source_thread_id: format!("thread-{id}"),
                project_id: Some(id % 10),
                project_link_kind: ProjectLinkKind::Automatic,
                source_cwd: PathBuf::from(format!("/project/{}", id % 10)),
                source_updated_at: id,
                last_seen_at: id,
                source_label: "cli".to_owned(),
                observed_status_label: "notLoaded".to_owned(),
                ephemeral: false,
                text_imported: false,
            })
            .collect::<Vec<_>>();
        let grouped = group_sessions(&sessions);
        assert_eq!(grouped.len(), 10);
        assert_eq!(grouped.values().map(Vec::len).sum::<usize>(), 1_000);
        assert!(grouped.values().all(|items| items.len() == 100));
    }
}
