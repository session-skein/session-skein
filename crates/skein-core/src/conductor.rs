//! Atomic, content-free routing receipts for one-prompt Codex dispatch.

use rusqlite::OptionalExtension;
use rusqlite::Transaction;
use rusqlite::TransactionBehavior;
use rusqlite::params;
use serde::Serialize;
use uuid::Uuid;

use crate::ControlPlan;
use crate::Error;
use crate::MatchConfidence;
use crate::MatchEvidence;
use crate::MatchOptions;
use crate::NewControlRun;
use crate::Registry;
use crate::Result;
use crate::WorkerClaim;
use crate::control::control_plan_on;
use crate::control::insert_control_plan;
use crate::insight::match_metadata_on;
use crate::registry::unix_timestamp;
use crate::worker::allocate_control_worker;

/// The preflight route identity that must remain stable through authenticated planning.
pub struct ExpectedConductorRoute<'a> {
    pub project_id: i64,
    pub action: &'a str,
    pub source_thread_id: Option<&'a str>,
}

/// Private one-prompt request used for atomic transactional re-matching and planning.
pub struct NewConductorRun<'a> {
    pub request_id: &'a str,
    pub prompt: &'a str,
    pub include_session_text: bool,
    pub full_access_acknowledged: bool,
    pub expected: ExpectedConductorRoute<'a>,
    pub explicit_selection: bool,
}

/// One selected candidate evidence contribution with no matched source value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConductorEvidence {
    pub scope: String,
    pub family: String,
    pub kind: String,
    pub points: i32,
    pub matches: usize,
}

/// Durable, content-free explanation of one accepted automatic route.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConductorDecision {
    pub id: i64,
    pub request_id: String,
    pub run_id: i64,
    pub created_at: i64,
    pub matched_at: i64,
    pub match_schema_version: u32,
    pub project_id: i64,
    pub source_thread_id: Option<String>,
    pub action: String,
    pub confidence: MatchConfidence,
    pub ambiguous: bool,
    pub resolution_kind: String,
    pub score: i32,
    pub runner_up_margin: i32,
    pub candidate_count: usize,
    pub query_bytes: usize,
    pub query_tokens: usize,
    pub include_session_text: bool,
    pub evidence: Vec<ConductorEvidence>,
    pub content_redacted: bool,
}

/// Atomic conductor planning result. Existing requests are status lookups, never replayed.
pub enum ConductorPlanOutcome {
    Created {
        decision: ConductorDecision,
        control: ControlPlan,
        worker: WorkerClaim,
    },
    Existing {
        decision: ConductorDecision,
        control: ControlPlan,
    },
}

impl Registry {
    /// Re-match and atomically bind an accepted route to policy, run, actions, and worker claim.
    pub fn plan_conductor_run(
        &mut self,
        input: &NewConductorRun<'_>,
    ) -> Result<ConductorPlanOutcome> {
        validate_request(input)?;
        let now = unix_timestamp();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(decision) = decision_by_request_id_on(&transaction, input.request_id)? {
            let control = control_plan_on(&transaction, decision.run_id)?;
            transaction.commit()?;
            return Ok(ConductorPlanOutcome::Existing { decision, control });
        }

        let report = match_metadata_on(
            &transaction,
            &MatchOptions {
                query: input.prompt,
                include_text: input.include_session_text,
                limit: if input.explicit_selection {
                    crate::MAX_MATCH_CANDIDATES
                } else {
                    1
                },
                now,
            },
            true,
        )?;
        let recommendation = report
            .recommendation
            .as_ref()
            .ok_or_else(|| Error::InvalidControlRequest("route_refused:no_match".to_owned()))?;
        if !input.explicit_selection
            && (recommendation.confidence != MatchConfidence::High || !recommendation.dispatchable)
        {
            return Err(Error::InvalidControlRequest(
                format!("route_refused:confidence_{:?}", recommendation.confidence).to_lowercase(),
            ));
        }
        if !input.explicit_selection && recommendation.ambiguous {
            return Err(Error::InvalidControlRequest(
                "route_refused:ambiguous".to_owned(),
            ));
        }
        let candidate = if input.explicit_selection {
            report
                .candidates
                .iter()
                .find(|candidate| candidate.project.id == input.expected.project_id)
        } else {
            report.candidates.first()
        }
        .ok_or_else(|| {
            Error::InvalidControlRequest("route_refused:selection_not_ranked".to_owned())
        })?;
        let selected_session = match input.expected.action {
            "start" => None,
            "resume" => {
                let thread_id = input.expected.source_thread_id.ok_or_else(|| {
                    Error::InvalidControlRequest("invalid expected conductor route".to_owned())
                })?;
                Some(
                    candidate
                        .suggested_session
                        .as_ref()
                        .filter(|session| {
                            session.source_thread_id == thread_id && session.resumable
                        })
                        .ok_or_else(|| {
                            Error::InvalidControlRequest(
                                "route_refused:session_not_ranked_or_resumable".to_owned(),
                            )
                        })?,
                )
            }
            _ => {
                return Err(Error::InvalidControlRequest(
                    "invalid expected conductor route".to_owned(),
                ));
            }
        };
        if !input.explicit_selection
            && (recommendation.project_id != input.expected.project_id
                || recommendation.action != input.expected.action
                || recommendation.source_thread_id.as_deref() != input.expected.source_thread_id)
        {
            return Err(Error::ControlStateConflict(
                "route_changed_after_authentication".to_owned(),
            ));
        }
        if let Some(session) = selected_session
            && session
                .evidence
                .iter()
                .any(|evidence| evidence.kind == "exact_thread")
            && matches!(
                session.resume_blocker.as_deref(),
                Some("active_run" | "recovery_required")
            )
        {
            return Err(Error::ControlStateConflict(format!(
                "thread_{}",
                session.resume_blocker.as_deref().unwrap_or("unavailable")
            )));
        }
        let evidence = selected_evidence(candidate);
        let evidence_score = evidence.iter().map(|item| item.points).sum::<i32>();
        if evidence_score != candidate.score {
            return Err(Error::ControlStateConflict(
                "selected route evidence did not sum to its score".to_owned(),
            ));
        }
        let control = insert_control_plan(
            &transaction,
            &NewControlRun {
                project_path: &candidate.project.path,
                resume_thread_id: input.expected.source_thread_id,
                prompt: input.prompt,
                full_access_acknowledged: input.full_access_acknowledged,
            },
            &crate::Project {
                id: candidate.project.id,
                name: candidate.project.name.clone(),
                path: candidate.project.path.clone(),
                updated_at: 0,
                metadata_refreshed_at: None,
                git: None,
            },
            now,
        )?;
        let decision = insert_decision(
            &transaction,
            input,
            &report,
            candidate,
            &evidence,
            control.run_id,
            now,
        )?;
        let worker = allocate_control_worker(&transaction, control.run_id, now)?;
        transaction.commit()?;
        Ok(ConductorPlanOutcome::Created {
            decision,
            control,
            worker,
        })
    }

    /// Look up one durable conductor receipt without exposing prompt content.
    pub fn conductor_decision_by_request_id(
        &self,
        request_id: &str,
    ) -> Result<Option<ConductorDecision>> {
        decision_by_request_id_on(&self.connection, request_id)
    }
}

fn validate_request(input: &NewConductorRun<'_>) -> Result<()> {
    if Uuid::parse_str(input.request_id).is_err() {
        return Err(Error::InvalidControlRequest(
            "conductor request id must be a UUID".to_owned(),
        ));
    }
    if !input.full_access_acknowledged {
        return Err(Error::InvalidControlRequest(
            "pass --full-access to authorize conductor execution".to_owned(),
        ));
    }
    if input.prompt.trim().is_empty() || input.prompt.len() > 64 * 1024 {
        return Err(Error::InvalidControlRequest(
            "conductor prompt must contain 1..=65536 bytes".to_owned(),
        ));
    }
    if !matches!(input.expected.action, "start" | "resume")
        || (input.expected.action == "start" && input.expected.source_thread_id.is_some())
        || (input.expected.action == "resume" && input.expected.source_thread_id.is_none())
    {
        return Err(Error::InvalidControlRequest(
            "invalid expected conductor route".to_owned(),
        ));
    }
    Ok(())
}

fn selected_evidence(candidate: &crate::ProjectMatch) -> Vec<ConductorEvidence> {
    candidate
        .evidence
        .iter()
        .map(|item| evidence("project", item))
        .chain(candidate.suggested_session.iter().flat_map(|session| {
            session
                .evidence
                .iter()
                .map(|item| evidence("session", item))
        }))
        .collect()
}

fn evidence(scope: &str, item: &MatchEvidence) -> ConductorEvidence {
    ConductorEvidence {
        scope: scope.to_owned(),
        family: item.family.clone(),
        kind: item.kind.clone(),
        points: item.points,
        matches: item.matches,
    }
}

fn insert_decision(
    transaction: &Transaction<'_>,
    input: &NewConductorRun<'_>,
    report: &crate::MatchReport,
    candidate: &crate::ProjectMatch,
    evidence: &[ConductorEvidence],
    run_id: i64,
    now: i64,
) -> Result<ConductorDecision> {
    let recommendation = report.recommendation.as_ref().ok_or_else(|| {
        Error::ControlStateConflict("accepted conductor report lost recommendation".to_owned())
    })?;
    transaction.execute(
        "INSERT INTO conductor_decisions (
            request_id, run_id, created_at, matched_at, match_schema_version, project_id,
            source_thread_id, action, confidence, ambiguous, resolution_kind, score,
            runner_up_margin, candidate_count, query_bytes, query_tokens, include_text
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                   ?13, ?14, ?15, ?16, ?17)",
        params![
            input.request_id,
            run_id,
            now,
            report.as_of,
            i64::from(report.schema_version),
            candidate.project.id,
            input.expected.source_thread_id,
            input.expected.action,
            format!("{:?}", recommendation.confidence).to_lowercase(),
            i64::from(recommendation.ambiguous),
            if input.explicit_selection {
                "user_selected"
            } else {
                "automatic"
            },
            candidate.score,
            recommendation.runner_up_margin,
            i64::try_from(report.candidate_count).unwrap_or(i64::MAX),
            i64::try_from(report.query_bytes).unwrap_or(i64::MAX),
            i64::try_from(report.query_token_count).unwrap_or(i64::MAX),
            i64::from(input.include_session_text),
        ],
    )?;
    let decision_id = transaction.last_insert_rowid();
    for (ordinal, item) in evidence.iter().enumerate() {
        transaction.execute(
            "INSERT INTO conductor_decision_evidence (
                decision_id, ordinal, scope, family, kind, points, matches
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                decision_id,
                i64::try_from(ordinal).unwrap_or(i64::MAX),
                item.scope,
                item.family,
                item.kind,
                item.points,
                i64::try_from(item.matches).unwrap_or(i64::MAX),
            ],
        )?;
    }
    decision_by_id_on(transaction, decision_id)?
        .ok_or_else(|| Error::ControlStateConflict("new conductor decision disappeared".to_owned()))
}

fn decision_by_request_id_on(
    connection: &rusqlite::Connection,
    request_id: &str,
) -> Result<Option<ConductorDecision>> {
    let id = connection
        .query_row(
            "SELECT id FROM conductor_decisions WHERE request_id = ?1",
            [request_id],
            |row| row.get(0),
        )
        .optional()?;
    id.map(|id| decision_by_id_on(connection, id))
        .transpose()
        .map(Option::flatten)
}

fn decision_by_id_on(
    connection: &rusqlite::Connection,
    id: i64,
) -> Result<Option<ConductorDecision>> {
    let base = connection
        .query_row(
            "SELECT id, request_id, run_id, created_at, matched_at, match_schema_version,
                project_id, source_thread_id, action, confidence, ambiguous, resolution_kind,
                score, runner_up_margin, candidate_count, query_bytes, query_tokens, include_text
             FROM conductor_decisions WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, i64>(15)?,
                    row.get::<_, i64>(16)?,
                    row.get::<_, i64>(17)?,
                ))
            },
        )
        .optional()?;
    let Some(base) = base else {
        return Ok(None);
    };
    let mut statement = connection.prepare(
        "SELECT scope, family, kind, points, matches
         FROM conductor_decision_evidence WHERE decision_id = ?1 ORDER BY ordinal",
    )?;
    let evidence = statement
        .query_map([id], |row| {
            Ok(ConductorEvidence {
                scope: row.get(0)?,
                family: row.get(1)?,
                kind: row.get(2)?,
                points: row.get(3)?,
                matches: usize::try_from(row.get::<_, i64>(4)?).unwrap_or(usize::MAX),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Some(ConductorDecision {
        id: base.0,
        request_id: base.1,
        run_id: base.2,
        created_at: base.3,
        matched_at: base.4,
        match_schema_version: u32::try_from(base.5).unwrap_or(u32::MAX),
        project_id: base.6,
        source_thread_id: base.7,
        action: base.8,
        confidence: match base.9.as_str() {
            "high" => MatchConfidence::High,
            "medium" => MatchConfidence::Medium,
            "low" => MatchConfidence::Low,
            _ => MatchConfidence::None,
        },
        ambiguous: base.10 != 0,
        resolution_kind: base.11,
        score: i32::try_from(base.12).unwrap_or(i32::MAX),
        runner_up_margin: i32::try_from(base.13).unwrap_or(i32::MAX),
        candidate_count: usize::try_from(base.14).unwrap_or(usize::MAX),
        query_bytes: usize::try_from(base.15).unwrap_or(usize::MAX),
        query_tokens: usize::try_from(base.16).unwrap_or(usize::MAX),
        include_session_text: base.17 != 0,
        evidence,
        content_redacted: true,
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use crate::ControlRunState;
    use crate::SessionObservation;
    use crate::SkeinPaths;

    use super::*;

    fn fixture() -> Result<(tempfile::TempDir, Registry, PathBuf, i64)> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary conductor fixture"),
            source,
        })?;
        let project = temp.path().join("alpha-renderer");
        fs::create_dir(&project).map_err(|source| Error::Io {
            path: project.clone(),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let registry = Registry::open(&paths)?;
        let project_id = registry.add_project(&project, Some("Alpha Renderer"))?.id;
        Ok((temp, registry, project, project_id))
    }

    fn start_input<'a>(
        request_id: &'a str,
        prompt: &'a str,
        project_id: i64,
    ) -> NewConductorRun<'a> {
        NewConductorRun {
            request_id,
            prompt,
            include_session_text: false,
            full_access_acknowledged: true,
            expected: ExpectedConductorRoute {
                project_id,
                action: "start",
                source_thread_id: None,
            },
            explicit_selection: false,
        }
    }

    fn count(registry: &Registry, table: &str) -> Result<i64> {
        registry
            .connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .map_err(Error::from)
    }

    fn import_session(
        registry: &mut Registry,
        project: std::path::PathBuf,
        thread_id: &str,
        updated_at: i64,
    ) -> Result<()> {
        registry.import_sessions(&[SessionObservation {
            source_kind: "codex".to_owned(),
            source_thread_id: thread_id.to_owned(),
            source_session_id: None,
            source_cwd: project,
            source_created_at: updated_at.saturating_sub(1),
            source_updated_at: updated_at,
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
        Ok(())
    }

    #[test]
    fn accepted_route_atomically_records_evidence_control_and_starting_worker() -> Result<()> {
        let (temp, mut registry, _project, project_id) = fixture()?;
        let request_id = "10000000-0000-4000-8000-000000000001";
        let prompt = "continue Alpha Renderer ultraviolet-sentinel";
        let outcome = registry.plan_conductor_run(&start_input(request_id, prompt, project_id))?;
        let ConductorPlanOutcome::Created {
            decision,
            control,
            worker,
        } = outcome
        else {
            panic!("first request must create a plan");
        };
        assert_eq!(decision.run_id, control.run_id);
        assert_eq!(worker.run_id(), control.run_id);
        assert_eq!(decision.confidence, MatchConfidence::High);
        assert_eq!(
            decision.score,
            decision
                .evidence
                .iter()
                .map(|item| item.points)
                .sum::<i32>()
        );
        assert_eq!(count(&registry, "control_policies")?, 1);
        assert_eq!(count(&registry, "control_runs")?, 1);
        assert_eq!(count(&registry, "control_turns")?, 1);
        assert_eq!(count(&registry, "control_actions")?, 2);
        assert_eq!(count(&registry, "conductor_decisions")?, 1);
        assert_eq!(count(&registry, "control_workers")?, 1);
        let run = registry.control_run(control.run_id)?.expect("run");
        assert_eq!(run.ownership_mode, "worker");
        registry
            .connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        let database =
            fs::read(temp.path().join("data").join("skein.sqlite3")).map_err(|source| {
                Error::Io {
                    path: temp.path().join("data").join("skein.sqlite3"),
                    source,
                }
            })?;
        assert!(
            !database
                .windows("ultraviolet-sentinel".len())
                .any(|window| { window == "ultraviolet-sentinel".as_bytes() })
        );
        Ok(())
    }

    #[test]
    fn request_id_retry_is_status_only_and_never_duplicates_state() -> Result<()> {
        let (_temp, mut registry, _project, project_id) = fixture()?;
        let request_id = "10000000-0000-4000-8000-000000000002";
        let first = registry.plan_conductor_run(&start_input(
            request_id,
            "continue Alpha Renderer first-private-input",
            project_id,
        ))?;
        let first_run = match first {
            ConductorPlanOutcome::Created { control, .. } => control.run_id,
            ConductorPlanOutcome::Existing { .. } => panic!("unexpected retry"),
        };
        let second = registry.plan_conductor_run(&start_input(
            request_id,
            "continue Alpha Renderer different-same-id-input",
            project_id,
        ))?;
        let ConductorPlanOutcome::Existing { decision, control } = second else {
            panic!("retry must be a lookup");
        };
        assert_eq!(decision.run_id, first_run);
        assert_eq!(control.run_id, first_run);
        assert_eq!(count(&registry, "control_runs")?, 1);
        assert_eq!(count(&registry, "control_workers")?, 1);
        Ok(())
    }

    #[test]
    fn ambiguous_route_and_transaction_failure_create_no_partial_control_state() -> Result<()> {
        let (temp, mut registry, project, project_id) = fixture()?;
        let second = temp.path().join("second");
        fs::create_dir(&second).map_err(|source| Error::Io {
            path: second.clone(),
            source,
        })?;
        registry.add_project(&project, Some("Shared Project"))?;
        registry.add_project(&second, Some("Shared Project"))?;
        let refused = registry.plan_conductor_run(&start_input(
            "10000000-0000-4000-8000-000000000003",
            "Shared Project",
            project_id,
        ));
        assert!(matches!(refused, Err(Error::InvalidControlRequest(_))));
        assert_eq!(count(&registry, "control_policies")?, 0);

        registry.add_project(&project, Some("Alpha Renderer"))?;
        registry.add_project(&second, Some("Second Project"))?;
        registry.connection.execute_batch(
            "CREATE TEMP TRIGGER fail_conductor_evidence
             BEFORE INSERT ON conductor_decision_evidence
             BEGIN SELECT RAISE(ABORT, 'synthetic evidence failure'); END;",
        )?;
        let failed = registry.plan_conductor_run(&start_input(
            "10000000-0000-4000-8000-000000000004",
            "continue Alpha Renderer",
            project_id,
        ));
        assert!(failed.is_err());
        for table in [
            "control_policies",
            "control_runs",
            "control_turns",
            "control_actions",
            "conductor_decisions",
            "control_workers",
        ] {
            assert_eq!(count(&registry, table)?, 0, "partial row in {table}");
        }
        Ok(())
    }

    #[test]
    fn explicit_ranked_project_resolves_ambiguity_without_reinterpreting_prompt() -> Result<()> {
        let (temp, mut registry, project, first_id) = fixture()?;
        let second = temp.path().join("second");
        fs::create_dir(&second).map_err(|source| Error::Io {
            path: second.clone(),
            source,
        })?;
        registry.add_project(&project, Some("Shared Project"))?;
        let second_id = registry.add_project(&second, Some("Shared Project"))?.id;

        let report = registry.match_conductor_metadata(&MatchOptions {
            query: "Shared Project",
            include_text: false,
            limit: 5,
            now: 1_000,
        })?;
        assert!(
            report
                .recommendation
                .as_ref()
                .is_some_and(|item| item.ambiguous)
        );
        assert!(report.resolution.required);
        assert_eq!(report.candidates.len(), 2);
        assert_eq!(report.candidates[0].rank, 1);
        assert_eq!(report.candidates[1].rank, 2);
        assert_eq!(count(&registry, "control_runs")?, 0);

        let outcome = registry.plan_conductor_run(&NewConductorRun {
            request_id: "10000000-0000-4000-8000-000000000013",
            prompt: "Shared Project",
            include_session_text: false,
            full_access_acknowledged: true,
            expected: ExpectedConductorRoute {
                project_id: second_id,
                action: "start",
                source_thread_id: None,
            },
            explicit_selection: true,
        })?;
        let ConductorPlanOutcome::Created { decision, .. } = outcome else {
            panic!("explicit resolution must create one plan");
        };
        assert_ne!(decision.project_id, first_id);
        assert_eq!(decision.project_id, second_id);
        assert!(decision.ambiguous);
        assert_eq!(decision.resolution_kind, "user_selected");
        assert_eq!(count(&registry, "control_runs")?, 1);
        Ok(())
    }

    #[test]
    fn explicit_non_top_routes_use_only_the_selected_project_and_thread() -> Result<()> {
        let (temp, mut registry, first, first_id) = fixture()?;
        let second = temp.path().join("second");
        fs::create_dir(&second).map_err(|source| Error::Io {
            path: second.clone(),
            source,
        })?;
        registry.add_project(&first, Some("Shared Work"))?;
        let second_id = registry.add_project(&second, Some("Shared Work"))?.id;
        let thread_a = "21000000-0000-4000-8000-000000000001";
        let thread_b = "22000000-0000-4000-8000-000000000002";
        import_session(&mut registry, first, thread_a, 100)?;
        import_session(&mut registry, second, thread_b, 90)?;
        let prompt = format!("Shared Work {thread_a}");
        let report = registry.match_conductor_metadata(&MatchOptions {
            query: &prompt,
            include_text: false,
            limit: 50,
            now: 100,
        })?;
        assert_eq!(report.candidates[0].project.id, first_id);
        assert_eq!(
            report
                .recommendation
                .as_ref()
                .and_then(|item| item.source_thread_id.as_deref()),
            Some(thread_a)
        );
        let selected = report
            .candidates
            .iter()
            .find(|candidate| candidate.project.id == second_id)
            .expect("lower-ranked second project");
        assert!(selected.score < report.candidates[0].score);
        assert_eq!(
            selected.selection.source_thread_id.as_deref(),
            Some(thread_b)
        );

        let started = registry.plan_conductor_run(&NewConductorRun {
            request_id: "10000000-0000-4000-8000-000000000014",
            prompt: &prompt,
            include_session_text: false,
            full_access_acknowledged: true,
            expected: ExpectedConductorRoute {
                project_id: second_id,
                action: "start",
                source_thread_id: None,
            },
            explicit_selection: true,
        })?;
        let ConductorPlanOutcome::Created {
            decision, control, ..
        } = started
        else {
            panic!("non-top explicit start must create");
        };
        assert_eq!(decision.project_id, second_id);
        assert_eq!(decision.source_thread_id, None);
        let started_run = registry
            .control_run(control.run_id)?
            .expect("started run remains durable");
        assert_eq!(started_run.project_id, second_id);
        assert_eq!(started_run.source_thread_id, None);

        let resumed = registry.plan_conductor_run(&NewConductorRun {
            request_id: "10000000-0000-4000-8000-000000000015",
            prompt: &prompt,
            include_session_text: false,
            full_access_acknowledged: true,
            expected: ExpectedConductorRoute {
                project_id: second_id,
                action: "resume",
                source_thread_id: Some(thread_b),
            },
            explicit_selection: true,
        })?;
        let ConductorPlanOutcome::Created {
            decision, control, ..
        } = resumed
        else {
            panic!("selected second session must resume");
        };
        assert_eq!(decision.source_thread_id.as_deref(), Some(thread_b));
        let resumed_run = registry
            .control_run(control.run_id)?
            .expect("resumed run remains durable");
        assert_eq!(resumed_run.source_thread_id.as_deref(), Some(thread_b));
        assert_ne!(resumed_run.source_thread_id.as_deref(), Some(thread_a));
        Ok(())
    }

    #[test]
    fn malformed_direct_routes_and_blocked_unselected_sessions_are_safe() -> Result<()> {
        let (_temp, mut registry, project, project_id) = fixture()?;
        let thread = "23000000-0000-4000-8000-000000000003";
        import_session(&mut registry, project.clone(), thread, 100)?;
        registry.plan_control_run(&NewControlRun {
            project_path: &project,
            resume_thread_id: Some(thread),
            prompt: "synthetic active owner",
            full_access_acknowledged: true,
        })?;
        let baseline = count(&registry, "control_runs")?;

        for (request_id, action, source_thread_id) in [
            ("10000000-0000-4000-8000-000000000016", "resume", None),
            (
                "10000000-0000-4000-8000-000000000017",
                "start",
                Some(thread),
            ),
            ("10000000-0000-4000-8000-000000000018", "unknown", None),
        ] {
            let result = registry.plan_conductor_run(&NewConductorRun {
                request_id,
                prompt: thread,
                include_session_text: false,
                full_access_acknowledged: true,
                expected: ExpectedConductorRoute {
                    project_id,
                    action,
                    source_thread_id,
                },
                explicit_selection: true,
            });
            assert!(matches!(result, Err(Error::InvalidControlRequest(_))));
            assert_eq!(count(&registry, "control_runs")?, baseline);
            assert_eq!(count(&registry, "conductor_decisions")?, 0);
        }

        let started = registry.plan_conductor_run(&NewConductorRun {
            request_id: "10000000-0000-4000-8000-000000000019",
            prompt: thread,
            include_session_text: false,
            full_access_acknowledged: true,
            expected: ExpectedConductorRoute {
                project_id,
                action: "start",
                source_thread_id: None,
            },
            explicit_selection: true,
        })?;
        let ConductorPlanOutcome::Created { control, .. } = started else {
            panic!("project-only start must ignore an unselected blocked session");
        };
        let started_run = registry
            .control_run(control.run_id)?
            .expect("project-only run remains durable");
        assert_eq!(started_run.source_thread_id, None);
        Ok(())
    }

    #[test]
    fn exact_thread_resumes_only_when_eligible_and_unowned() -> Result<()> {
        let (_temp, mut registry, project, project_id) = fixture()?;
        let thread_id = "20000000-0000-4000-8000-000000000001";
        registry.import_sessions(&[SessionObservation {
            source_kind: "codex".to_owned(),
            source_thread_id: thread_id.to_owned(),
            source_session_id: None,
            source_cwd: project,
            source_created_at: 1,
            source_updated_at: 2,
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
        let input = NewConductorRun {
            request_id: "10000000-0000-4000-8000-000000000005",
            prompt: thread_id,
            include_session_text: false,
            full_access_acknowledged: true,
            expected: ExpectedConductorRoute {
                project_id,
                action: "resume",
                source_thread_id: Some(thread_id),
            },
            explicit_selection: false,
        };
        let outcome = registry.plan_conductor_run(&input)?;
        let ConductorPlanOutcome::Created { decision, .. } = outcome else {
            panic!("first exact thread request must create");
        };
        assert_eq!(decision.action, "resume");
        assert_eq!(decision.source_thread_id.as_deref(), Some(thread_id));

        let new_thread = registry.plan_conductor_run(&NewConductorRun {
            request_id: "10000000-0000-4000-8000-000000000006",
            prompt: thread_id,
            include_session_text: false,
            full_access_acknowledged: true,
            expected: ExpectedConductorRoute {
                project_id,
                action: "start",
                source_thread_id: None,
            },
            explicit_selection: false,
        })?;
        let ConductorPlanOutcome::Created { decision, .. } = new_thread else {
            panic!("a blocked prior thread must permit the recommended new-thread start");
        };
        assert_eq!(decision.action, "start");
        assert_eq!(decision.source_thread_id, None);
        assert_eq!(count(&registry, "conductor_decisions")?, 2);
        Ok(())
    }

    #[test]
    fn completed_skein_owned_thread_resumes_before_session_sync() -> Result<()> {
        let (_temp, mut registry, project, project_id) = fixture()?;
        let initial = registry.plan_control_run(&crate::NewControlRun {
            project_path: &project,
            resume_thread_id: None,
            prompt: "private initial",
            full_access_acknowledged: true,
        })?;
        registry.begin_control_action(initial.thread_action_id)?;
        registry.acknowledge_thread_action(
            initial.thread_action_id,
            "owned-thread",
            Some("owned-session"),
        )?;
        registry.begin_control_action(initial.turn_action_id)?;
        registry.acknowledge_turn_action(initial.turn_action_id, "owned-turn")?;
        registry.complete_control_run(initial.run_id, "completed")?;
        assert!(registry.list_sessions()?.is_empty());

        let report = registry.match_conductor_metadata(&MatchOptions {
            query: "owned-thread",
            include_text: false,
            limit: 5,
            now: crate::registry::unix_timestamp(),
        })?;
        let recommendation = report.recommendation.expect("owned thread route");
        assert_eq!(recommendation.action, "resume");
        assert_eq!(
            recommendation.source_thread_id.as_deref(),
            Some("owned-thread")
        );
        let other_project = project
            .parent()
            .expect("fixture parent")
            .join("other-project");
        fs::create_dir(&other_project).map_err(|source| Error::Io {
            path: other_project.clone(),
            source,
        })?;
        registry.add_project(&other_project, Some("Other Project"))?;
        assert!(matches!(
            registry.plan_control_run(&crate::NewControlRun {
                project_path: &other_project,
                resume_thread_id: Some("owned-thread"),
                prompt: "private wrong-project resume",
                full_access_acknowledged: true,
            }),
            Err(Error::InvalidControlRequest(_))
        ));
        let outcome = registry.plan_conductor_run(&NewConductorRun {
            request_id: "10000000-0000-4000-8000-000000000008",
            prompt: "owned-thread",
            include_session_text: false,
            full_access_acknowledged: true,
            expected: ExpectedConductorRoute {
                project_id,
                action: "resume",
                source_thread_id: Some("owned-thread"),
            },
            explicit_selection: false,
        })?;
        let ConductorPlanOutcome::Created {
            decision, control, ..
        } = outcome
        else {
            panic!("owned thread must create resume plan");
        };
        assert_eq!(decision.action, "resume");
        assert_eq!(decision.source_thread_id.as_deref(), Some("owned-thread"));
        let (session_id, source_thread_id): (Option<i64>, Option<String>) =
            registry.connection.query_row(
                "SELECT session_id, source_thread_id FROM control_runs WHERE id = ?1",
                [control.run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
        assert!(session_id.is_none());
        assert_eq!(source_thread_id.as_deref(), Some("owned-thread"));
        let blocked = registry.match_conductor_metadata(&MatchOptions {
            query: "owned-thread",
            include_text: false,
            limit: 5,
            now: crate::registry::unix_timestamp(),
        })?;
        let blocked_session = blocked.candidates[0]
            .suggested_session
            .as_ref()
            .expect("blocked owned thread");
        assert_eq!(
            blocked_session.resume_blocker.as_deref(),
            Some("active_run")
        );
        assert!(!blocked_session.resumable);
        Ok(())
    }

    #[test]
    fn expired_pre_dispatch_conductor_worker_fails_without_recovery_claim() -> Result<()> {
        let (_temp, mut registry, _project, project_id) = fixture()?;
        let outcome = registry.plan_conductor_run(&start_input(
            "10000000-0000-4000-8000-000000000007",
            "continue Alpha Renderer",
            project_id,
        ))?;
        let run_id = match outcome {
            ConductorPlanOutcome::Created { control, .. } => control.run_id,
            ConductorPlanOutcome::Existing { .. } => panic!("unexpected retry"),
        };
        registry.connection.execute(
            "UPDATE control_workers SET lease_acquired_at = 0, lease_expires_at = 0,
                heartbeat_at = 0 WHERE run_id = ?1",
            [run_id],
        )?;
        assert_eq!(registry.recover_expired_workers()?, vec![run_id]);
        let run = registry.control_run(run_id)?.expect("failed run");
        assert_eq!(run.state, ControlRunState::Failed);
        assert_ne!(run.state, ControlRunState::RecoveryRequired);
        Ok(())
    }
}
