use std::path::Path;
use std::path::PathBuf;

use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::Transaction;
use rusqlite::params;
use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;

use crate::Error;
use crate::Project;
use crate::Registry;
use crate::Result;
use crate::WorkerClaim;
use crate::registry::unix_timestamp;
use crate::worker::assert_worker_fence;

/// Skein-owned lifecycle state for one controlled Codex run.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlRunState {
    Planned,
    Starting,
    Active,
    Completed,
    Failed,
    Interrupted,
    RecoveryRequired,
}

impl ControlRunState {
    fn from_str(value: &str) -> rusqlite::Result<Self> {
        match value {
            "planned" => Ok(Self::Planned),
            "starting" => Ok(Self::Starting),
            "active" => Ok(Self::Active),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "interrupted" => Ok(Self::Interrupted),
            "recovery_required" => Ok(Self::RecoveryRequired),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}

/// Audited app-server mutation kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlActionKind {
    ThreadStart,
    ThreadResume,
    TurnStart,
    TurnSteer,
    TurnInterrupt,
    StatusReconcile,
}

impl ControlActionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::ThreadStart => "thread_start",
            Self::ThreadResume => "thread_resume",
            Self::TurnStart => "turn_start",
            Self::TurnSteer => "turn_steer",
            Self::TurnInterrupt => "turn_interrupt",
            Self::StatusReconcile => "status_reconcile",
        }
    }

    fn method(self) -> &'static str {
        match self {
            Self::ThreadStart => "thread/start",
            Self::ThreadResume => "thread/resume",
            Self::TurnStart => "turn/start",
            Self::TurnSteer => "turn/steer",
            Self::TurnInterrupt => "turn/interrupt",
            Self::StatusReconcile => "thread/read",
        }
    }

    fn from_str(value: &str) -> rusqlite::Result<Self> {
        match value {
            "thread_start" => Ok(Self::ThreadStart),
            "thread_resume" => Ok(Self::ThreadResume),
            "turn_start" => Ok(Self::TurnStart),
            "turn_steer" => Ok(Self::TurnSteer),
            "turn_interrupt" => Ok(Self::TurnInterrupt),
            "status_reconcile" => Ok(Self::StatusReconcile),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}

/// Durable state of one audited control mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlActionState {
    Planned,
    Dispatching,
    Acknowledged,
    Succeeded,
    Failed,
    Uncertain,
}

impl ControlActionState {
    fn from_str(value: &str) -> rusqlite::Result<Self> {
        match value {
            "planned" => Ok(Self::Planned),
            "dispatching" => Ok(Self::Dispatching),
            "acknowledged" => Ok(Self::Acknowledged),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "uncertain" => Ok(Self::Uncertain),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}

/// Public redaction-safe run projection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlRun {
    pub id: i64,
    pub run_key: String,
    pub project_id: i64,
    pub project_name: String,
    pub working_directory: PathBuf,
    pub state: ControlRunState,
    pub ownership_mode: String,
    pub source_thread_id: Option<String>,
    pub source_session_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub terminal_at: Option<i64>,
    pub sandbox_mode: String,
    pub approval_mode: String,
    pub network_access: bool,
    pub full_access_acknowledged_at: i64,
}

/// One action in a run's append-only audit history.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ControlAction {
    pub id: i64,
    pub action_key: String,
    pub run_id: i64,
    pub action_kind: ControlActionKind,
    pub state: ControlActionState,
    pub request_method: String,
    pub request_fingerprint: String,
    pub source_result_id: Option<String>,
    pub client_request_id: Option<String>,
    pub input_bytes: Option<usize>,
    pub expected_source_turn_id: Option<String>,
    pub created_at: i64,
    pub dispatch_started_at: Option<i64>,
    pub terminal_at: Option<i64>,
}

/// Private detail for an explicitly selected run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ControlRunDetail {
    #[serde(flatten)]
    pub run: ControlRun,
    pub input_bytes: usize,
    pub terminal_condition_version: u32,
    pub source_turn_id: Option<String>,
    pub actions: Vec<ControlAction>,
    pub content_redacted: bool,
}

/// Validated intent used to create a run before any Codex mutation.
pub struct NewControlRun<'a> {
    pub project_path: &'a Path,
    pub resume_thread_id: Option<&'a str>,
    pub prompt: &'a str,
    pub full_access_acknowledged: bool,
}

/// Identifiers needed by the protocol layer after atomic planning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlPlan {
    pub run_id: i64,
    pub run_key: String,
    pub thread_action_id: i64,
    pub turn_action_id: i64,
    pub client_message_id: String,
    pub working_directory: PathBuf,
}

/// Exact audited interruption request for one active worker-owned turn.
pub struct InterruptPlan {
    pub action_id: i64,
    pub should_dispatch: bool,
    pub thread_id: String,
    pub turn_id: String,
}

/// Idempotent audited same-turn steering request owned by one live worker.
pub struct SteerPlan {
    pub action_id: i64,
    pub should_dispatch: bool,
    pub thread_id: String,
    pub turn_id: String,
    pub client_request_id: String,
}

/// One audited, read-only recovery probe for a lost worker run.
pub struct ReconciliationPlan {
    pub action_id: i64,
    pub should_dispatch: bool,
    pub thread_id: String,
    pub expected_turn_id: Option<String>,
    pub initial_client_message_id: String,
}

/// Content-free authoritative evidence extracted from Codex `thread/read`.
pub struct ReconciliationObservation<'a> {
    pub thread_id: &'a str,
    pub thread_status: &'a str,
    pub turn_id: Option<&'a str>,
    pub turn_status: Option<&'a str>,
    pub initial_message_observed: bool,
    pub observed_steer_client_ids: &'a [&'a str],
}

impl Registry {
    /// Persist policy, run, turn, actions, and initial audit events atomically.
    pub fn plan_control_run(&mut self, input: &NewControlRun<'_>) -> Result<ControlPlan> {
        validate_control_input(input)?;
        let project = self.get_project(input.project_path)?;
        let now = unix_timestamp();
        let transaction = self.connection.transaction()?;
        let plan = insert_control_plan(&transaction, input, &project, now)?;
        transaction.commit()?;
        Ok(plan)
    }

    /// Reconstruct the content-free execution plan for a previously planned run.
    pub fn control_plan(&self, run_id: i64) -> Result<ControlPlan> {
        control_plan_on(&self.connection, run_id)
    }

    /// Persist one idempotent interrupt intent for the exact active turn.
    pub fn plan_owned_interrupt(
        &mut self,
        run_id: i64,
        claim: &WorkerClaim,
    ) -> Result<InterruptPlan> {
        if claim.run_id() != run_id {
            return Err(Error::ControlStateConflict(
                "worker cannot interrupt another run".to_owned(),
            ));
        }
        let now = unix_timestamp();
        let transaction = self.connection.transaction()?;
        assert_worker_fence(&transaction, claim, &["busy"])?;
        let (thread_id, turn_id, control_turn_id, policy_id): (String, String, i64, i64) =
            transaction.query_row(
                "SELECT r.source_thread_id, t.source_turn_id, t.id, r.policy_id
                 FROM control_runs r JOIN control_turns t ON t.run_id = r.id
                 WHERE r.id = ?1 AND r.state = 'active' AND r.ownership_mode = 'worker'
                 AND t.state = 'running' AND t.turn_number = 1",
                [run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        let existing: Option<(i64, String)> = transaction
            .query_row(
                "SELECT id, state FROM control_actions
                 WHERE run_id = ?1 AND action_kind = 'turn_interrupt'",
                [run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((action_id, state)) = existing {
            transaction.commit()?;
            return Ok(InterruptPlan {
                action_id,
                should_dispatch: state == "planned",
                thread_id,
                turn_id,
            });
        }
        let action_id = insert_action(
            &transaction,
            run_id,
            Some(control_turn_id),
            policy_id,
            ControlActionKind::TurnInterrupt,
            "turn/interrupt:exact-active-turn",
            now,
        )?;
        transaction.commit()?;
        Ok(InterruptPlan {
            action_id,
            should_dispatch: true,
            thread_id,
            turn_id,
        })
    }

    /// Persist an idempotent steer intent for the exact active worker-owned turn.
    pub fn plan_owned_steer(
        &mut self,
        run_id: i64,
        client_request_id: &str,
        input_bytes: usize,
        claim: &WorkerClaim,
    ) -> Result<SteerPlan> {
        if claim.run_id() != run_id {
            return Err(Error::ControlStateConflict(
                "worker cannot steer another run".to_owned(),
            ));
        }
        if Uuid::parse_str(client_request_id).is_err() {
            return Err(Error::InvalidControlRequest(
                "steer request id must be a UUID".to_owned(),
            ));
        }
        if input_bytes == 0 {
            return Err(Error::InvalidControlRequest(
                "steer input must be non-empty".to_owned(),
            ));
        }
        let now = unix_timestamp();
        let transaction = self.connection.transaction()?;
        assert_worker_fence(&transaction, claim, &["ready", "busy"])?;
        let existing: Option<(i64, String, String, i64)> = transaction
            .query_row(
                "SELECT a.id, a.expected_source_turn_id, r.source_thread_id, a.input_bytes
                 FROM control_actions a JOIN control_runs r ON r.id = a.run_id
                 WHERE a.run_id = ?1 AND a.client_request_id = ?2
                 AND a.action_kind = 'turn_steer'",
                params![run_id, client_request_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        if let Some((action_id, expected_turn_id, thread_id, stored_input_bytes)) = existing {
            if stored_input_bytes != i64::try_from(input_bytes).unwrap_or(i64::MAX) {
                return Err(Error::ControlStateConflict(
                    "steer request id was already used with a different input length".to_owned(),
                ));
            }
            transaction.commit()?;
            return Ok(SteerPlan {
                action_id,
                should_dispatch: false,
                thread_id,
                turn_id: expected_turn_id,
                client_request_id: client_request_id.to_owned(),
            });
        }
        assert_worker_fence(&transaction, claim, &["busy"])?;
        let (thread_id, turn_id, control_turn_id, policy_id): (String, String, i64, i64) =
            transaction.query_row(
                "SELECT r.source_thread_id, t.source_turn_id, t.id, r.policy_id
                 FROM control_runs r JOIN control_turns t ON t.run_id = r.id
                 WHERE r.id = ?1 AND r.state = 'active' AND r.ownership_mode = 'worker'
                 AND t.state = 'running' AND t.turn_number = 1",
                [run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        let interrupt_barrier: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM control_actions WHERE run_id = ?1
             AND action_kind = 'turn_interrupt'
             AND state IN ('planned', 'dispatching', 'acknowledged', 'succeeded')",
            [run_id],
            |row| row.get(0),
        )?;
        if interrupt_barrier != 0 {
            return Err(Error::ControlStateConflict(
                "worker cannot queue steer after interrupt".to_owned(),
            ));
        }
        let action_id = insert_action(
            &transaction,
            run_id,
            Some(control_turn_id),
            policy_id,
            ControlActionKind::TurnSteer,
            "turn/steer:text",
            now,
        )?;
        transaction.execute(
            "UPDATE control_actions SET client_request_id = ?1, input_bytes = ?2,
                expected_source_turn_id = ?3 WHERE id = ?4",
            params![
                client_request_id,
                i64::try_from(input_bytes).unwrap_or(i64::MAX),
                turn_id,
                action_id
            ],
        )?;
        transaction.commit()?;
        Ok(SteerPlan {
            action_id,
            should_dispatch: true,
            thread_id,
            turn_id,
            client_request_id: client_request_id.to_owned(),
        })
    }

    /// Record that Codex accepted an exact worker-owned steer request.
    pub fn acknowledge_owned_steer(
        &mut self,
        action_id: i64,
        turn_id: &str,
        claim: &WorkerClaim,
    ) -> Result<()> {
        let now = unix_timestamp();
        let transaction = self.connection.transaction()?;
        assert_worker_fence(&transaction, claim, &["busy"])?;
        assert_owned_action(&transaction, action_id, claim)?;
        let expected: String = transaction.query_row(
            "SELECT expected_source_turn_id FROM control_actions
             WHERE id = ?1 AND action_kind = 'turn_steer'",
            [action_id],
            |row| row.get(0),
        )?;
        if expected != turn_id {
            return Err(Error::ControlStateConflict(
                "Codex acknowledged a different turn for steer".to_owned(),
            ));
        }
        transition_action(
            &transaction,
            action_id,
            "dispatching",
            "succeeded",
            now,
            Some(turn_id),
        )?;
        append_event(&transaction, action_id, "succeeded", now, None)?;
        transaction.commit()?;
        Ok(())
    }

    /// Record an authoritative Codex rejection of a dispatched steer.
    pub fn reject_owned_steer(&mut self, action_id: i64, claim: &WorkerClaim) -> Result<()> {
        let now = unix_timestamp();
        let transaction = self.connection.transaction()?;
        assert_worker_fence(&transaction, claim, &["busy"])?;
        assert_owned_action(&transaction, action_id, claim)?;
        transition_action(&transaction, action_id, "dispatching", "failed", now, None)?;
        transaction.execute(
            "UPDATE control_actions SET error_class = 'steer_rejected',
                error_message = 'Codex rejected steer for the exact active turn'
             WHERE id = ?1",
            [action_id],
        )?;
        append_event(&transaction, action_id, "failed", now, None)?;
        transaction.commit()?;
        Ok(())
    }

    /// Plan an idempotent read-only reconciliation for a lost worker run.
    pub fn plan_reconciliation(
        &mut self,
        run_id: i64,
        client_request_id: &str,
    ) -> Result<ReconciliationPlan> {
        if Uuid::parse_str(client_request_id).is_err() {
            return Err(Error::InvalidControlRequest(
                "reconciliation request id must be a UUID".to_owned(),
            ));
        }
        let now = unix_timestamp();
        let transaction = self.connection.transaction()?;
        let existing: Option<(i64, String, String, Option<String>, String)> = transaction
            .query_row(
                "SELECT a.id, a.state, r.source_thread_id, a.expected_source_turn_id,
                    t.client_message_id
                 FROM control_actions a JOIN control_runs r ON r.id = a.run_id
                 JOIN control_turns t ON t.run_id = r.id AND t.turn_number = 1
                 WHERE a.run_id = ?1 AND a.client_request_id = ?2
                 AND a.action_kind = 'status_reconcile'",
                params![run_id, client_request_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        if let Some((action_id, state, thread_id, expected_turn_id, initial_client_message_id)) =
            existing
        {
            transaction.commit()?;
            return Ok(ReconciliationPlan {
                action_id,
                should_dispatch: state == "planned",
                thread_id,
                expected_turn_id,
                initial_client_message_id,
            });
        }
        let (thread_id, expected_turn_id, initial_client_message_id, control_turn_id, policy_id): (
            String,
            Option<String>,
            String,
            i64,
            i64,
        ) = transaction.query_row(
            "SELECT r.source_thread_id, t.source_turn_id, t.client_message_id, t.id, r.policy_id
             FROM control_runs r JOIN control_turns t ON t.run_id = r.id
             JOIN control_workers w ON w.run_id = r.id
             WHERE r.id = ?1 AND r.state = 'recovery_required'
             AND r.ownership_mode = 'worker' AND w.state IN ('lost', 'exited')",
            [run_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        let action_id = insert_action(
            &transaction,
            run_id,
            Some(control_turn_id),
            policy_id,
            ControlActionKind::StatusReconcile,
            "thread/read:identity-status-only",
            now,
        )?;
        transaction.execute(
            "UPDATE control_actions SET client_request_id = ?1,
                expected_source_turn_id = ?2 WHERE id = ?3",
            params![client_request_id, expected_turn_id, action_id],
        )?;
        transaction.commit()?;
        Ok(ReconciliationPlan {
            action_id,
            should_dispatch: true,
            thread_id,
            expected_turn_id,
            initial_client_message_id,
        })
    }

    /// Enter the dispatch boundary for a read-only recovery probe.
    pub fn begin_reconciliation(&mut self, action_id: i64) -> Result<()> {
        let now = unix_timestamp();
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE control_actions SET state = 'dispatching', dispatch_started_at = ?1
             WHERE id = ?2 AND action_kind = 'status_reconcile' AND state = 'planned'
             AND worker_id IS NULL AND EXISTS (
                SELECT 1 FROM control_runs r JOIN control_workers w ON w.run_id = r.id
                WHERE r.id = control_actions.run_id AND r.state = 'recovery_required'
                AND r.ownership_mode = 'worker' AND w.state IN ('lost', 'exited')
             )",
            params![now, action_id],
        )?;
        if changed != 1 {
            return Err(Error::ControlStateConflict(
                "reconciliation action was not dispatchable".to_owned(),
            ));
        }
        append_event(&transaction, action_id, "dispatching", now, None)?;
        transaction.commit()?;
        Ok(())
    }

    /// Persist content-free source evidence and apply only exact terminal truth.
    pub fn record_reconciliation(
        &mut self,
        action_id: i64,
        observation: &ReconciliationObservation<'_>,
    ) -> Result<ControlRun> {
        let now = unix_timestamp();
        let transaction = self.connection.transaction()?;
        let (run_id, expected_thread_id, expected_turn_id): (i64, String, Option<String>) =
            transaction.query_row(
                "SELECT a.run_id, r.source_thread_id, a.expected_source_turn_id
                 FROM control_actions a JOIN control_runs r ON r.id = a.run_id
                 WHERE a.id = ?1 AND a.action_kind = 'status_reconcile'
                 AND a.state = 'dispatching' AND r.state = 'recovery_required'",
                [action_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        if expected_thread_id != observation.thread_id {
            return Err(Error::ControlStateConflict(
                "thread/read returned a different thread identity".to_owned(),
            ));
        }
        if let (Some(observed), Some(expected)) = (observation.turn_id, expected_turn_id.as_deref())
            && observed != expected
        {
            return Err(Error::ControlStateConflict(
                "reconciliation observation did not target the recorded turn".to_owned(),
            ));
        }
        if expected_turn_id.is_none()
            && observation.turn_id.is_some()
            && !observation.initial_message_observed
        {
            return Err(Error::ControlStateConflict(
                "turn identity could not be proven by the initial client message id".to_owned(),
            ));
        }
        let effective_turn_id = expected_turn_id.as_deref().or(observation.turn_id);
        let stored_thread_status = match observation.thread_status {
            "notLoaded" | "not_loaded" => "not_loaded",
            "idle" => "idle",
            "systemError" | "system_error" => "system_error",
            "active" => "active",
            _ => "unknown",
        };
        let (outcome, terminal, stored_turn_status) = match observation.turn_status {
            None => ("turn_missing", None, None),
            Some("inProgress" | "in_progress") => ("turn_still_running", None, Some("in_progress")),
            Some("completed") => (
                "terminal_confirmed",
                Some(("completed", "completed")),
                Some("completed"),
            ),
            Some("failed") => (
                "terminal_confirmed",
                Some(("failed", "failed")),
                Some("failed"),
            ),
            Some("interrupted") => (
                "terminal_confirmed",
                Some(("interrupted", "interrupted")),
                Some("interrupted"),
            ),
            Some(_) => ("unsupported_status", None, Some("unknown")),
        };
        transaction.execute(
            "INSERT INTO control_reconciliations (
                action_id, run_id, observed_at, source_thread_id, expected_source_turn_id,
                thread_status, turn_found, observed_turn_status, initial_message_observed,
                steer_messages_observed, outcome
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                action_id,
                run_id,
                now,
                observation.thread_id,
                effective_turn_id,
                stored_thread_status,
                i64::from(observation.turn_id.is_some()),
                stored_turn_status,
                i64::from(observation.initial_message_observed),
                i64::try_from(observation.observed_steer_client_ids.len()).unwrap_or(i64::MAX),
                outcome
            ],
        )?;
        if expected_turn_id.is_none()
            && let Some(turn_id) = effective_turn_id
        {
            transaction.execute(
                "UPDATE control_turns SET source_turn_id = ?1
                 WHERE run_id = ?2 AND source_turn_id IS NULL",
                params![turn_id, run_id],
            )?;
            transaction.execute(
                "UPDATE control_actions SET expected_source_turn_id = ?1 WHERE id = ?2",
                params![turn_id, action_id],
            )?;
        }
        transition_action(
            &transaction,
            action_id,
            "dispatching",
            "succeeded",
            now,
            observation.turn_id,
        )?;
        append_event(&transaction, action_id, outcome, now, None)?;
        if let Some((run_state, turn_state)) = terminal {
            let turn_changed = transaction.execute(
                "UPDATE control_turns SET state = ?1, terminal_at = ?2
                 WHERE run_id = ?3 AND state = 'uncertain'",
                params![turn_state, now, run_id],
            )?;
            let run_changed = transaction.execute(
                "UPDATE control_runs SET state = ?1, terminal_at = ?2, updated_at = ?2,
                    last_error_class = NULL, last_error_message = NULL
                 WHERE id = ?3 AND state = 'recovery_required'",
                params![run_state, now, run_id],
            )?;
            if turn_changed != 1 || run_changed != 1 {
                return Err(Error::ControlStateConflict(
                    "terminal reconciliation lost the recovery state".to_owned(),
                ));
            }
        }
        transaction.commit()?;
        self.control_run(run_id)?.ok_or_else(|| {
            Error::ControlStateConflict(format!("run {run_id} disappeared after reconciliation"))
        })
    }

    /// Fail a read-only reconciliation without persisting source error content.
    pub fn fail_reconciliation(&mut self, action_id: i64) -> Result<()> {
        let now = unix_timestamp();
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE control_actions SET state = 'failed', terminal_at = ?1,
                error_class = 'source_read_failed',
                error_message = 'Codex source status could not be read safely'
             WHERE id = ?2 AND action_kind = 'status_reconcile' AND state = 'dispatching'",
            params![now, action_id],
        )?;
        if changed != 1 {
            return Err(Error::ControlStateConflict(
                "reconciliation action was not dispatching".to_owned(),
            ));
        }
        append_event(&transaction, action_id, "failed", now, None)?;
        transaction.commit()?;
        Ok(())
    }

    /// Record that Codex accepted an exact worker-owned interrupt request.
    pub fn acknowledge_owned_interrupt(
        &mut self,
        action_id: i64,
        turn_id: &str,
        claim: &WorkerClaim,
    ) -> Result<()> {
        let now = unix_timestamp();
        let transaction = self.connection.transaction()?;
        assert_worker_fence(&transaction, claim, &["busy"])?;
        assert_owned_action(&transaction, action_id, claim)?;
        transition_action(
            &transaction,
            action_id,
            "dispatching",
            "acknowledged",
            now,
            Some(turn_id),
        )?;
        append_event(&transaction, action_id, "acknowledged", now, None)?;
        transaction.commit()?;
        Ok(())
    }

    /// Mark an audited action dispatching immediately before its protocol write.
    pub fn begin_control_action(&mut self, action_id: i64) -> Result<()> {
        self.begin_control_action_inner(action_id, None)
    }

    /// Begin an action only while the exact worker lease fence remains current.
    pub fn begin_owned_control_action(
        &mut self,
        action_id: i64,
        claim: &WorkerClaim,
    ) -> Result<()> {
        self.begin_control_action_inner(action_id, Some(claim))
    }

    fn begin_control_action_inner(
        &mut self,
        action_id: i64,
        claim: Option<&WorkerClaim>,
    ) -> Result<()> {
        let now = unix_timestamp();
        let transaction = self.connection.transaction()?;
        let (run_id, turn_id, action_kind): (i64, Option<i64>, String) = transaction.query_row(
            "SELECT run_id, turn_id, action_kind FROM control_actions WHERE id = ?1",
            [action_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let action_kind_value = ControlActionKind::from_str(&action_kind)?;
        if let Some(claim) = claim {
            if claim.run_id() != run_id {
                return Err(Error::ControlStateConflict(
                    "worker cannot dispatch an action from another run".to_owned(),
                ));
            }
            assert_worker_fence(&transaction, claim, &["ready", "busy"])?;
        } else {
            assert_run_ownership(&transaction, run_id, "foreground")?;
        }
        let changed = match claim {
            Some(claim) => transaction.execute(
                "UPDATE control_actions SET state = 'dispatching', dispatch_started_at = ?1,
                    worker_id = ?3, worker_lease_epoch = ?4
                 WHERE id = ?2 AND state = 'planned' AND worker_id IS NULL",
                params![now, action_id, claim.id(), claim.lease_epoch()],
            )?,
            None => transaction.execute(
                "UPDATE control_actions SET state = 'dispatching', dispatch_started_at = ?1
                 WHERE id = ?2 AND state = 'planned' AND worker_id IS NULL",
                params![now, action_id],
            )?,
        };
        if changed != 1 {
            return Err(Error::ControlStateConflict(format!(
                "action {action_id} was not planned"
            )));
        }
        if let Some(turn_id) = turn_id
            && action_kind_value == ControlActionKind::TurnStart
        {
            let turn_changed = transaction.execute(
                "UPDATE control_turns SET state = 'dispatching'
                 WHERE id = ?1 AND state = 'planned'",
                [turn_id],
            )?;
            if turn_changed != 1 {
                return Err(Error::ControlStateConflict(format!(
                    "turn for action {action_id} was not planned"
                )));
            }
        }
        let run_changed = match action_kind_value {
            ControlActionKind::ThreadStart | ControlActionKind::ThreadResume => transaction
                .execute(
                    "UPDATE control_runs SET state = 'starting', updated_at = ?1
                     WHERE id = ?2 AND state = 'planned'",
                    params![now, run_id],
                )?,
            ControlActionKind::TurnStart => transaction.execute(
                "UPDATE control_runs SET updated_at = ?1
                 WHERE id = ?2 AND state = 'starting'
                 AND EXISTS (
                    SELECT 1 FROM control_actions
                    WHERE run_id = ?2 AND action_kind IN ('thread_start', 'thread_resume')
                    AND state = 'succeeded'
                 )",
                params![now, run_id],
            )?,
            ControlActionKind::TurnSteer | ControlActionKind::TurnInterrupt => transaction
                .execute(
                    "UPDATE control_runs SET updated_at = ?1
                     WHERE id = ?2 AND state = 'active'",
                    params![now, run_id],
                )?,
            ControlActionKind::StatusReconcile => transaction.execute(
                "UPDATE control_runs SET updated_at = ?1
                 WHERE id = ?2 AND state = 'recovery_required'",
                params![now, run_id],
            )?,
        };
        if run_changed != 1 {
            return Err(Error::ControlStateConflict(format!(
                "run {run_id} was not in the required state for {action_kind}"
            )));
        }
        append_event(&transaction, action_id, "dispatching", now, None)?;
        transaction.commit()?;
        Ok(())
    }

    /// Record the effective thread identity after start or resume acknowledgement.
    pub fn acknowledge_thread_action(
        &mut self,
        action_id: i64,
        source_thread_id: &str,
        source_session_id: Option<&str>,
    ) -> Result<()> {
        self.acknowledge_thread_action_inner(action_id, source_thread_id, source_session_id, None)
    }

    /// Record a thread acknowledgement under the exact current worker fence.
    pub fn acknowledge_owned_thread_action(
        &mut self,
        action_id: i64,
        source_thread_id: &str,
        source_session_id: Option<&str>,
        claim: &WorkerClaim,
    ) -> Result<()> {
        self.acknowledge_thread_action_inner(
            action_id,
            source_thread_id,
            source_session_id,
            Some(claim),
        )
    }

    fn acknowledge_thread_action_inner(
        &mut self,
        action_id: i64,
        source_thread_id: &str,
        source_session_id: Option<&str>,
        claim: Option<&WorkerClaim>,
    ) -> Result<()> {
        let now = unix_timestamp();
        let transaction = self.connection.transaction()?;
        let run_id: i64 = transaction.query_row(
            "SELECT run_id FROM control_actions WHERE id = ?1",
            [action_id],
            |row| row.get(0),
        )?;
        if let Some(claim) = claim {
            assert_worker_fence(&transaction, claim, &["busy"])?;
            assert_owned_action(&transaction, action_id, claim)?;
        } else {
            assert_run_ownership(&transaction, run_id, "foreground")?;
        }
        transition_action(
            &transaction,
            action_id,
            "dispatching",
            "succeeded",
            now,
            Some(source_thread_id),
        )?;
        let run_changed = transaction.execute(
            "UPDATE control_runs SET source_thread_id = ?1, source_session_id = ?2,
                state = 'starting', updated_at = ?3
             WHERE id = ?4 AND state = 'starting'
             AND (source_thread_id IS NULL OR source_thread_id = ?1)",
            params![source_thread_id, source_session_id, now, run_id],
        )?;
        if run_changed != 1 {
            return Err(Error::ControlStateConflict(format!(
                "run {run_id} was not awaiting a thread acknowledgement"
            )));
        }
        append_event(&transaction, action_id, "succeeded", now, None)?;
        transaction.commit()?;
        Ok(())
    }

    /// Record the accepted turn ID and active Skein-owned run state.
    pub fn acknowledge_turn_action(&mut self, action_id: i64, source_turn_id: &str) -> Result<()> {
        self.acknowledge_turn_action_inner(action_id, source_turn_id, None)
    }

    /// Record a turn acknowledgement under the exact current worker fence.
    pub fn acknowledge_owned_turn_action(
        &mut self,
        action_id: i64,
        source_turn_id: &str,
        claim: &WorkerClaim,
    ) -> Result<()> {
        self.acknowledge_turn_action_inner(action_id, source_turn_id, Some(claim))
    }

    fn acknowledge_turn_action_inner(
        &mut self,
        action_id: i64,
        source_turn_id: &str,
        claim: Option<&WorkerClaim>,
    ) -> Result<()> {
        let now = unix_timestamp();
        let transaction = self.connection.transaction()?;
        let (run_id, turn_id): (i64, i64) = transaction.query_row(
            "SELECT run_id, turn_id FROM control_actions WHERE id = ?1",
            [action_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if let Some(claim) = claim {
            assert_worker_fence(&transaction, claim, &["busy"])?;
            assert_owned_action(&transaction, action_id, claim)?;
        } else {
            assert_run_ownership(&transaction, run_id, "foreground")?;
        }
        transition_action(
            &transaction,
            action_id,
            "dispatching",
            "acknowledged",
            now,
            Some(source_turn_id),
        )?;
        let turn_changed = transaction.execute(
            "UPDATE control_turns SET state = 'running', source_turn_id = ?1
             WHERE id = ?2 AND state = 'dispatching'",
            params![source_turn_id, turn_id],
        )?;
        if turn_changed != 1 {
            return Err(Error::ControlStateConflict(format!(
                "turn for action {action_id} was not dispatching"
            )));
        }
        let run_changed = transaction.execute(
            "UPDATE control_runs SET state = 'active', updated_at = ?1
             WHERE id = ?2 AND state = 'starting'",
            params![now, run_id],
        )?;
        if run_changed != 1 {
            return Err(Error::ControlStateConflict(format!(
                "run {run_id} was not awaiting a turn acknowledgement"
            )));
        }
        append_event(&transaction, action_id, "acknowledged", now, None)?;
        transaction.commit()?;
        Ok(())
    }

    /// Finalize an authoritative `turn/completed` outcome.
    pub fn complete_control_run(&mut self, run_id: i64, status: &str) -> Result<ControlRun> {
        self.complete_control_run_inner(run_id, status, None)
    }

    /// Finalize a run only under the exact current worker fence.
    pub fn complete_owned_control_run(
        &mut self,
        run_id: i64,
        status: &str,
        claim: &WorkerClaim,
    ) -> Result<ControlRun> {
        self.complete_control_run_inner(run_id, status, Some(claim))
    }

    fn complete_control_run_inner(
        &mut self,
        run_id: i64,
        status: &str,
        claim: Option<&WorkerClaim>,
    ) -> Result<ControlRun> {
        let (run_state, turn_state) = match status {
            "completed" => ("completed", "completed"),
            "interrupted" => ("interrupted", "interrupted"),
            "failed" => ("failed", "failed"),
            _ => {
                return Err(Error::InvalidControlRequest(format!(
                    "non-terminal Codex turn status: {status}"
                )));
            }
        };
        let now = unix_timestamp();
        let transaction = self.connection.transaction()?;
        if let Some(claim) = claim {
            if claim.run_id() != run_id {
                return Err(Error::ControlStateConflict(
                    "worker cannot complete another run".to_owned(),
                ));
            }
            assert_worker_fence(&transaction, claim, &["busy"])?;
        } else {
            assert_run_ownership(&transaction, run_id, "foreground")?;
        }
        let turn_changed = transaction.execute(
            "UPDATE control_turns SET state = ?1, terminal_at = ?2
             WHERE run_id = ?3 AND state = 'running'",
            params![turn_state, now, run_id],
        )?;
        if turn_changed != 1 {
            return Err(Error::ControlStateConflict(format!(
                "run {run_id} did not have exactly one running turn"
            )));
        }
        let action_id: i64 = transaction.query_row(
            "SELECT id FROM control_actions WHERE run_id = ?1 AND action_kind = 'turn_start'",
            [run_id],
            |row| row.get(0),
        )?;
        if let Some(claim) = claim {
            assert_owned_action(&transaction, action_id, claim)?;
        }
        transition_action(
            &transaction,
            action_id,
            "acknowledged",
            "succeeded",
            now,
            None,
        )?;
        append_event(&transaction, action_id, status, now, None)?;
        let interrupt_action: Option<i64> = transaction
            .query_row(
                "SELECT id FROM control_actions WHERE run_id = ?1
                 AND action_kind = 'turn_interrupt' AND state = 'acknowledged'",
                [run_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(interrupt_action) = interrupt_action {
            if let Some(claim) = claim {
                assert_owned_action(&transaction, interrupt_action, claim)?;
            }
            let interrupt_state = if status == "interrupted" {
                "succeeded"
            } else {
                "failed"
            };
            transition_action(
                &transaction,
                interrupt_action,
                "acknowledged",
                interrupt_state,
                now,
                None,
            )?;
            if interrupt_state == "failed" {
                transaction.execute(
                    "UPDATE control_actions SET error_class = 'interrupt_raced',
                        error_message = 'turn reached another terminal outcome before interruption'
                     WHERE id = ?1",
                    [interrupt_action],
                )?;
            }
            append_event(&transaction, interrupt_action, status, now, None)?;
        }
        transaction.execute(
            "UPDATE control_actions SET state = 'failed', terminal_at = ?1,
                error_class = 'interrupt_raced',
                error_message = 'turn reached a terminal outcome before interruption dispatch'
             WHERE run_id = ?2 AND action_kind = 'turn_interrupt' AND state = 'planned'",
            params![now, run_id],
        )?;
        transaction.execute(
            "INSERT INTO control_action_events (action_id, sequence, event_kind, recorded_at, detail)
             SELECT a.id,
                COALESCE((SELECT MAX(e.sequence) FROM control_action_events e
                          WHERE e.action_id = a.id), 0) + 1,
                'failed', ?1, NULL
             FROM control_actions a WHERE a.run_id = ?2
             AND a.action_kind = 'turn_interrupt' AND a.error_class = 'interrupt_raced'
             AND NOT EXISTS (
                SELECT 1 FROM control_action_events e
                WHERE e.action_id = a.id AND e.event_kind = 'failed'
             )",
            params![now, run_id],
        )?;
        transaction.execute(
            "UPDATE control_actions SET state = 'failed', terminal_at = ?1,
                error_class = 'turn_already_terminal',
                error_message = 'turn completed before queued steer dispatch'
             WHERE run_id = ?2 AND action_kind = 'turn_steer' AND state = 'planned'",
            params![now, run_id],
        )?;
        transaction.execute(
            "INSERT INTO control_action_events (action_id, sequence, event_kind, recorded_at, detail)
             SELECT a.id,
                COALESCE((SELECT MAX(e.sequence) FROM control_action_events e
                          WHERE e.action_id = a.id), 0) + 1,
                'failed', ?1, NULL
             FROM control_actions a WHERE a.run_id = ?2
             AND a.action_kind = 'turn_steer' AND a.error_class = 'turn_already_terminal'",
            params![now, run_id],
        )?;
        let run_changed = transaction.execute(
            "UPDATE control_runs SET state = ?1, updated_at = ?2, terminal_at = ?2
             WHERE id = ?3 AND state = 'active'",
            params![run_state, now, run_id],
        )?;
        if run_changed != 1 {
            return Err(Error::ControlStateConflict(format!(
                "run {run_id} was not active at completion"
            )));
        }
        transaction.commit()?;
        self.control_run(run_id)?.ok_or_else(|| {
            Error::ControlStateConflict(format!("run {run_id} disappeared after completion"))
        })
    }

    /// Mark a post-dispatch failure uncertain so it is never replayed automatically.
    pub fn mark_control_uncertain(&mut self, run_id: i64) -> Result<()> {
        self.mark_control_uncertain_inner(run_id, None)
    }

    /// Quarantine ambiguous mutations only under the exact current worker fence.
    pub fn mark_owned_control_uncertain(&mut self, run_id: i64, claim: &WorkerClaim) -> Result<()> {
        self.mark_control_uncertain_inner(run_id, Some(claim))
    }

    fn mark_control_uncertain_inner(
        &mut self,
        run_id: i64,
        claim: Option<&WorkerClaim>,
    ) -> Result<()> {
        let now = unix_timestamp();
        let transaction = self.connection.transaction()?;
        if let Some(claim) = claim {
            if claim.run_id() != run_id {
                return Err(Error::ControlStateConflict(
                    "worker cannot quarantine another run".to_owned(),
                ));
            }
            assert_worker_fence(&transaction, claim, &["busy"])?;
        } else {
            assert_run_ownership(&transaction, run_id, "foreground")?;
        }
        transaction.execute(
            "UPDATE control_actions SET state = 'failed', terminal_at = ?1,
                error_class = 'steer_input_lost',
                error_message = 'steer text was not persisted and cannot be replayed'
             WHERE run_id = ?2 AND action_kind = 'turn_steer' AND state = 'planned'",
            params![now, run_id],
        )?;
        transaction.execute(
            "INSERT INTO control_action_events (action_id, sequence, event_kind, recorded_at, detail)
             SELECT a.id,
                COALESCE((SELECT MAX(e.sequence) FROM control_action_events e
                          WHERE e.action_id = a.id), 0) + 1,
                'failed', ?1, NULL
             FROM control_actions a WHERE a.run_id = ?2
             AND a.action_kind = 'turn_steer' AND a.error_class = 'steer_input_lost'",
            params![now, run_id],
        )?;
        let mut statement = transaction.prepare(
            "SELECT id FROM control_actions WHERE run_id = ?1
             AND state IN ('dispatching', 'acknowledged')",
        )?;
        let ids = statement
            .query_map([run_id], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        for action_id in ids {
            transaction.execute(
                "UPDATE control_actions SET state = 'uncertain', terminal_at = ?1,
                    error_class = 'transport_uncertain',
                    error_message = 'protocol acknowledgement was not durably observed'
                 WHERE id = ?2",
                params![now, action_id],
            )?;
            append_event(&transaction, action_id, "uncertain", now, None)?;
        }
        transaction.execute(
            "UPDATE control_turns SET state = 'uncertain', terminal_at = ?1
             WHERE run_id = ?2 AND state IN ('dispatching', 'running')",
            params![now, run_id],
        )?;
        transaction.execute(
            "UPDATE control_runs SET state = 'recovery_required', updated_at = ?1,
                last_error_class = 'transport_uncertain',
                last_error_message = 'read-only reconciliation is required before retry'
             WHERE id = ?2 AND state NOT IN ('completed', 'failed', 'interrupted')",
            params![now, run_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Convert abandoned in-flight mutations into explicit recovery candidates.
    pub fn mark_stale_control_runs(&mut self, force: bool) -> Result<Vec<ControlRun>> {
        if !force {
            return Err(Error::InvalidControlRequest(
                "marking in-flight control state stale requires explicit force acknowledgement"
                    .to_owned(),
            ));
        }
        let run_ids = {
            let mut statement = self.connection.prepare(
                "SELECT id FROM control_runs
                 WHERE ownership_mode = 'foreground'
                 AND state IN ('planned', 'starting', 'active')
                 UNION
                 SELECT DISTINCT a.run_id FROM control_actions a
                 JOIN control_runs r ON r.id = a.run_id
                 WHERE r.ownership_mode = 'foreground'
                 AND a.state IN ('dispatching', 'acknowledged')",
            )?;
            statement
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for run_id in &run_ids {
            self.mark_control_uncertain(*run_id)?;
        }
        run_ids
            .into_iter()
            .map(|id| {
                self.control_run(id)?.ok_or_else(|| {
                    Error::ControlStateConflict(format!("recovery run {id} disappeared"))
                })
            })
            .collect()
    }

    pub fn list_control_runs(&self) -> Result<Vec<ControlRun>> {
        list_control_runs_on(&self.connection)
    }

    pub fn control_run(&self, id: i64) -> Result<Option<ControlRun>> {
        self.connection
            .query_row(&format!("{RUN_SELECT} WHERE r.id = ?1"), [id], run_from_row)
            .optional()
            .map_err(Error::from)
    }

    pub fn control_run_detail(&self, id: i64) -> Result<ControlRunDetail> {
        let run = self.control_run(id)?.ok_or_else(|| {
            Error::InvalidControlRequest(format!("control run {id} was not found"))
        })?;
        let (input_bytes, terminal_condition_version, source_turn_id) = self.connection.query_row(
            "SELECT input_bytes, terminal_condition_version, source_turn_id
                 FROM control_turns WHERE run_id = ?1 AND turn_number = 1",
            [id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get(2)?)),
        )?;
        let mut statement = self.connection.prepare(
            "SELECT id, action_key, run_id, action_kind, state, request_method,
                request_fingerprint, source_result_id, created_at, dispatch_started_at, terminal_at,
                client_request_id, input_bytes, expected_source_turn_id
             FROM control_actions WHERE run_id = ?1 ORDER BY created_at, id",
        )?;
        let actions = statement
            .query_map([id], action_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(ControlRunDetail {
            run,
            input_bytes: usize::try_from(input_bytes).unwrap_or(usize::MAX),
            terminal_condition_version: u32::try_from(terminal_condition_version)
                .unwrap_or(u32::MAX),
            source_turn_id,
            actions,
            content_redacted: true,
        })
    }
}

const RUN_SELECT: &str = "SELECT r.id, r.run_key, r.project_id, p.name,
    r.working_directory, r.state, r.source_thread_id, r.source_session_id,
    r.created_at, r.updated_at, r.terminal_at, cp.sandbox_mode, cp.approval_mode,
    cp.network_access, cp.acknowledged_at, r.ownership_mode
    FROM control_runs r JOIN projects p ON p.id = r.project_id
    JOIN control_policies cp ON cp.id = r.policy_id";

pub(crate) fn list_control_runs_on(connection: &Connection) -> Result<Vec<ControlRun>> {
    let mut statement = connection.prepare(&format!(
        "{RUN_SELECT} ORDER BY r.updated_at DESC, r.id DESC"
    ))?;
    statement
        .query_map([], run_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Error::from)
}

pub(crate) fn validate_control_input(input: &NewControlRun<'_>) -> Result<()> {
    if !input.full_access_acknowledged {
        return Err(Error::InvalidControlRequest(
            "full-access execution requires explicit acknowledgement".to_owned(),
        ));
    }
    if input.prompt.trim().is_empty() {
        return Err(Error::InvalidControlRequest(
            "prompt must be non-empty".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn insert_control_plan(
    transaction: &Transaction<'_>,
    input: &NewControlRun<'_>,
    project: &Project,
    now: i64,
) -> Result<ControlPlan> {
    validate_control_input(input)?;
    if !project.path.is_absolute() {
        return Err(Error::InvalidControlRequest(
            "control working directory must be absolute".to_owned(),
        ));
    }
    let cwd = project.path.to_str().ok_or_else(|| {
        Error::InvalidControlRequest("control path must be valid UTF-8".to_owned())
    })?;
    let run_key = Uuid::new_v4().to_string();
    let client_message_id = Uuid::new_v4().to_string();
    transaction.execute(
        "INSERT INTO control_policies (
            created_at, sandbox_mode, approval_mode, network_access, project_id,
            working_directory, acknowledged_at, acknowledgement_source
         ) VALUES (?1, 'danger_full_access', 'never', 1, ?2, ?3, ?1, 'cli_flag')",
        params![now, project.id, cwd],
    )?;
    let policy_id = transaction.last_insert_rowid();
    let session_id = match input.resume_thread_id {
        Some(thread_id) => validated_resume_session_id(transaction, thread_id, project.id)?,
        None => None,
    };
    transaction.execute(
        "INSERT INTO control_runs (
            run_key, project_id, session_id, policy_id, runtime_kind,
            working_directory, state, source_thread_id, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, 'codex', ?5, 'planned', ?6, ?7, ?7)",
        params![
            run_key,
            project.id,
            session_id,
            policy_id,
            cwd,
            input.resume_thread_id,
            now
        ],
    )?;
    let run_id = transaction.last_insert_rowid();
    transaction.execute(
        "INSERT INTO control_turns (
            run_id, turn_number, state, input_bytes, terminal_condition_version,
            client_message_id, created_at
         ) VALUES (?1, 1, 'planned', ?2, 1, ?3, ?4)",
        params![
            run_id,
            i64::try_from(input.prompt.len()).unwrap_or(i64::MAX),
            client_message_id,
            now
        ],
    )?;
    let turn_id = transaction.last_insert_rowid();
    let thread_kind = if input.resume_thread_id.is_some() {
        ControlActionKind::ThreadResume
    } else {
        ControlActionKind::ThreadStart
    };
    let thread_action_id = insert_action(
        transaction,
        run_id,
        None,
        policy_id,
        thread_kind,
        &format!("{}:project:{}", thread_kind.method(), project.id),
        now,
    )?;
    let turn_action_id = insert_action(
        transaction,
        run_id,
        Some(turn_id),
        policy_id,
        ControlActionKind::TurnStart,
        "turn/start:text",
        now,
    )?;
    Ok(ControlPlan {
        run_id,
        run_key,
        thread_action_id,
        turn_action_id,
        client_message_id,
        working_directory: project.path.clone(),
    })
}

pub(crate) fn control_plan_on(connection: &Connection, run_id: i64) -> Result<ControlPlan> {
    let (run_key, client_message_id, working_directory): (String, String, String) = connection
        .query_row(
            "SELECT r.run_key, t.client_message_id, r.working_directory
             FROM control_runs r JOIN control_turns t ON t.run_id = r.id
             WHERE r.id = ?1 AND t.turn_number = 1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    let thread_action_id = connection.query_row(
        "SELECT id FROM control_actions WHERE run_id = ?1
         AND action_kind IN ('thread_start', 'thread_resume')",
        [run_id],
        |row| row.get(0),
    )?;
    let turn_action_id = connection.query_row(
        "SELECT id FROM control_actions WHERE run_id = ?1 AND action_kind = 'turn_start'",
        [run_id],
        |row| row.get(0),
    )?;
    Ok(ControlPlan {
        run_id,
        run_key,
        thread_action_id,
        turn_action_id,
        client_message_id,
        working_directory: PathBuf::from(working_directory),
    })
}

fn insert_action(
    transaction: &Transaction<'_>,
    run_id: i64,
    turn_id: Option<i64>,
    policy_id: i64,
    kind: ControlActionKind,
    request_fingerprint: &str,
    now: i64,
) -> Result<i64> {
    let key = Uuid::new_v4().to_string();
    transaction.execute(
        "INSERT INTO control_actions (
            action_key, run_id, turn_id, policy_id, action_kind, state,
            request_method, request_fingerprint, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'planned', ?6, ?7, ?8)",
        params![
            key,
            run_id,
            turn_id,
            policy_id,
            kind.as_str(),
            kind.method(),
            request_fingerprint,
            now
        ],
    )?;
    let id = transaction.last_insert_rowid();
    append_event(transaction, id, "planned", now, None)?;
    Ok(id)
}

fn append_event(
    transaction: &Transaction<'_>,
    action_id: i64,
    kind: &str,
    now: i64,
    detail: Option<&str>,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO control_action_events (action_id, sequence, event_kind, recorded_at, detail)
         VALUES (?1,
            COALESCE((SELECT MAX(sequence) + 1 FROM control_action_events WHERE action_id = ?1), 1),
            ?2, ?3, ?4)",
        params![action_id, kind, now, detail],
    )?;
    Ok(())
}

fn transition_action(
    transaction: &Transaction<'_>,
    id: i64,
    expected: &str,
    next: &str,
    now: i64,
    result_id: Option<&str>,
) -> Result<()> {
    let changed = transaction.execute(
        "UPDATE control_actions SET state = ?1, source_result_id = COALESCE(?2, source_result_id),
            terminal_at = CASE WHEN ?1 IN ('succeeded', 'failed', 'uncertain') THEN ?3 ELSE NULL END
         WHERE id = ?4 AND state = ?5",
        params![next, result_id, now, id, expected],
    )?;
    if changed != 1 {
        return Err(Error::ControlStateConflict(format!(
            "action {id} was not {expected}"
        )));
    }
    Ok(())
}

fn assert_owned_action(
    transaction: &Transaction<'_>,
    action_id: i64,
    claim: &WorkerClaim,
) -> Result<()> {
    let count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM control_actions
         WHERE id = ?1 AND run_id = ?2 AND worker_id = ?3 AND worker_lease_epoch = ?4",
        params![action_id, claim.run_id(), claim.id(), claim.lease_epoch()],
        |row| row.get(0),
    )?;
    if count != 1 {
        return Err(Error::ControlStateConflict(
            "action is not owned by the current worker fence".to_owned(),
        ));
    }
    Ok(())
}

fn assert_run_ownership(transaction: &Transaction<'_>, run_id: i64, expected: &str) -> Result<()> {
    let count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM control_runs WHERE id = ?1 AND ownership_mode = ?2",
        params![run_id, expected],
        |row| row.get(0),
    )?;
    if count != 1 {
        return Err(Error::ControlStateConflict(format!(
            "run {run_id} is not owned by {expected} control"
        )));
    }
    Ok(())
}

fn validated_resume_session_id(
    transaction: &Transaction<'_>,
    thread_id: &str,
    expected_project_id: i64,
) -> Result<Option<i64>> {
    let competing: Option<String> = transaction
        .query_row(
            "SELECT state FROM control_runs WHERE source_thread_id = ?1
             AND state IN ('planned', 'starting', 'active', 'recovery_required')
             ORDER BY id DESC LIMIT 1",
            [thread_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(state) = competing {
        let class = if state == "recovery_required" {
            "thread_recovery_required"
        } else {
            "thread_already_active"
        };
        return Err(Error::ControlStateConflict(format!(
            "{class}: resume thread {thread_id} already has a nonterminal Skein run"
        )));
    }
    let session = transaction
        .query_row(
            "SELECT id, project_id, project_link_kind, status_label, source_label, ephemeral
             FROM sessions
             WHERE source_kind = 'codex' AND source_thread_id = ?1",
            [thread_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)? != 0,
                ))
            },
        )
        .optional()?;
    let Some((session_id, project_id, link_kind, status, source_label, ephemeral)) = session else {
        let owned: Option<(Option<i64>, i64)> = transaction
            .query_row(
                "SELECT session_id, project_id FROM control_runs
                 WHERE source_thread_id = ?1
                 AND state IN ('completed', 'failed', 'interrupted')
                 ORDER BY updated_at DESC, id DESC LIMIT 1",
                [thread_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((owned_session_id, owned_project_id)) = owned else {
            return Err(Error::InvalidControlRequest(format!(
                "resume thread {thread_id} is absent from the session catalog and completed Skein runs"
            )));
        };
        if owned_project_id != expected_project_id {
            return Err(Error::InvalidControlRequest(format!(
                "resume thread {thread_id} is not bound to the selected project"
            )));
        }
        return Ok(owned_session_id);
    };
    if project_id != Some(expected_project_id) {
        return Err(Error::InvalidControlRequest(format!(
            "resume thread {thread_id} is not bound to the selected project"
        )));
    }
    if ephemeral
        || status == "systemError"
        || source_label.starts_with("subAgent")
        || !matches!(link_kind.as_str(), "automatic" | "manual")
    {
        return Err(Error::InvalidControlRequest(format!(
            "resume thread {thread_id} is not eligible for controlled resume"
        )));
    }
    Ok(Some(session_id))
}

fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ControlRun> {
    Ok(ControlRun {
        id: row.get(0)?,
        run_key: row.get(1)?,
        project_id: row.get(2)?,
        project_name: row.get(3)?,
        working_directory: PathBuf::from(row.get::<_, String>(4)?),
        state: ControlRunState::from_str(&row.get::<_, String>(5)?)?,
        ownership_mode: row.get(15)?,
        source_thread_id: row.get(6)?,
        source_session_id: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        terminal_at: row.get(10)?,
        sandbox_mode: row.get(11)?,
        approval_mode: row.get(12)?,
        network_access: row.get::<_, i64>(13)? != 0,
        full_access_acknowledged_at: row.get(14)?,
    })
}

fn action_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ControlAction> {
    Ok(ControlAction {
        id: row.get(0)?,
        action_key: row.get(1)?,
        run_id: row.get(2)?,
        action_kind: ControlActionKind::from_str(&row.get::<_, String>(3)?)?,
        state: ControlActionState::from_str(&row.get::<_, String>(4)?)?,
        request_method: row.get(5)?,
        request_fingerprint: row.get(6)?,
        source_result_id: row.get(7)?,
        created_at: row.get(8)?,
        dispatch_started_at: row.get(9)?,
        terminal_at: row.get(10)?,
        client_request_id: row.get(11)?,
        input_bytes: row
            .get::<_, Option<i64>>(12)?
            .map(|value| usize::try_from(value).unwrap_or(usize::MAX)),
        expected_source_turn_id: row.get(13)?,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::SessionObservation;
    use crate::SkeinPaths;

    use super::*;

    fn registry() -> Result<(tempfile::TempDir, Registry, PathBuf)> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary test directory"),
            source,
        })?;
        let project = temp.path().join("project");
        fs::create_dir(&project).map_err(|source| Error::Io {
            path: project.clone(),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let registry = Registry::open(&paths)?;
        registry.add_project(&project, Some("Synthetic Project"))?;
        Ok((temp, registry, project))
    }

    fn input<'a>(project: &'a Path, acknowledged: bool) -> NewControlRun<'a> {
        NewControlRun {
            project_path: project,
            resume_thread_id: None,
            prompt: "Sensitive synthetic prompt",
            full_access_acknowledged: acknowledged,
        }
    }

    fn active_worker_run(
        registry: &mut Registry,
        project: &Path,
    ) -> Result<(ControlPlan, WorkerClaim)> {
        let plan = registry.plan_control_run(&input(project, true))?;
        let claim = registry.create_control_worker(plan.run_id)?;
        registry.mark_worker_ready(&claim, "127.0.0.1:12345", 42)?;
        registry.heartbeat_worker(&claim, crate::WorkerState::Busy)?;
        registry.begin_owned_control_action(plan.thread_action_id, &claim)?;
        registry.acknowledge_owned_thread_action(
            plan.thread_action_id,
            "synthetic-thread",
            Some("synthetic-session"),
            &claim,
        )?;
        registry.begin_owned_control_action(plan.turn_action_id, &claim)?;
        registry.acknowledge_owned_turn_action(plan.turn_action_id, "synthetic-turn", &claim)?;
        Ok((plan, claim))
    }

    fn catalog_thread(registry: &mut Registry, project: &Path, thread_id: &str) -> Result<()> {
        registry.import_sessions(&[SessionObservation {
            source_kind: "codex".to_owned(),
            source_thread_id: thread_id.to_owned(),
            source_session_id: Some("synthetic-session".to_owned()),
            source_cwd: project.to_path_buf(),
            source_created_at: 10,
            source_updated_at: 20,
            source_label: "cli".to_owned(),
            observed_status_label: "idle".to_owned(),
            model_provider: Some("openai".to_owned()),
            source_version: Some("1.2.3".to_owned()),
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
    fn refuses_unacknowledged_full_access_without_writing() -> Result<()> {
        let (_temp, mut registry, project) = registry()?;
        assert!(matches!(
            registry.plan_control_run(&input(&project, false)),
            Err(Error::InvalidControlRequest(_))
        ));
        assert!(registry.list_control_runs()?.is_empty());
        Ok(())
    }

    #[test]
    fn plans_policy_run_turn_actions_and_events_atomically_without_prompt_storage() -> Result<()> {
        let (_temp, mut registry, project) = registry()?;
        let plan = registry.plan_control_run(&input(&project, true))?;
        let detail = registry.control_run_detail(plan.run_id)?;
        assert_eq!(detail.run.state, ControlRunState::Planned);
        assert_eq!(detail.run.sandbox_mode, "danger_full_access");
        assert_eq!(detail.run.approval_mode, "never");
        assert!(detail.run.network_access);
        assert_eq!(detail.actions.len(), 2);
        assert_eq!(detail.input_bytes, "Sensitive synthetic prompt".len());
        assert!(detail.content_redacted);

        let serialized = serde_json::to_string(&detail)
            .map_err(|error| Error::InvalidControlRequest(error.to_string()))?;
        assert!(!serialized.contains("Sensitive synthetic prompt"));
        assert_eq!(detail.terminal_condition_version, 1);
        let columns: String = registry.connection.query_row(
            "SELECT group_concat(name, ',') FROM pragma_table_info('control_turns')",
            [],
            |row| row.get(0),
        )?;
        assert!(!columns.contains("input_text"));
        assert!(!columns.contains("prompt"));
        Ok(())
    }

    #[test]
    fn records_dispatch_acknowledgement_and_terminal_completion() -> Result<()> {
        let (_temp, mut registry, project) = registry()?;
        let plan = registry.plan_control_run(&input(&project, true))?;
        registry.begin_control_action(plan.thread_action_id)?;
        registry.acknowledge_thread_action(
            plan.thread_action_id,
            "synthetic-thread",
            Some("synthetic-session"),
        )?;
        registry.begin_control_action(plan.turn_action_id)?;
        registry.acknowledge_turn_action(plan.turn_action_id, "synthetic-turn")?;
        let completed = registry.complete_control_run(plan.run_id, "completed")?;
        assert_eq!(completed.state, ControlRunState::Completed);
        assert_eq!(
            completed.source_thread_id.as_deref(),
            Some("synthetic-thread")
        );
        let detail = registry.control_run_detail(plan.run_id)?;
        assert_eq!(detail.source_turn_id.as_deref(), Some("synthetic-turn"));
        assert!(
            detail
                .actions
                .iter()
                .all(|action| action.state == ControlActionState::Succeeded)
        );
        Ok(())
    }

    #[test]
    fn accepts_each_authoritative_terminal_turn_status() -> Result<()> {
        for (status, expected) in [
            ("completed", ControlRunState::Completed),
            ("failed", ControlRunState::Failed),
            ("interrupted", ControlRunState::Interrupted),
        ] {
            let (_temp, mut registry, project) = registry()?;
            let plan = registry.plan_control_run(&input(&project, true))?;
            registry.begin_control_action(plan.thread_action_id)?;
            registry.acknowledge_thread_action(
                plan.thread_action_id,
                "synthetic-thread",
                Some("synthetic-session"),
            )?;
            registry.begin_control_action(plan.turn_action_id)?;
            registry.acknowledge_turn_action(plan.turn_action_id, "synthetic-turn")?;
            assert_eq!(
                registry.complete_control_run(plan.run_id, status)?.state,
                expected
            );
        }
        Ok(())
    }

    #[test]
    fn recovery_marks_dispatching_mutations_uncertain_without_replay() -> Result<()> {
        let (_temp, mut registry, project) = registry()?;
        let plan = registry.plan_control_run(&input(&project, true))?;
        registry.begin_control_action(plan.thread_action_id)?;
        let recovered = registry.mark_stale_control_runs(true)?;
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].state, ControlRunState::RecoveryRequired);
        let detail = registry.control_run_detail(plan.run_id)?;
        assert_eq!(detail.actions[0].state, ControlActionState::Uncertain);
        assert_eq!(detail.actions[1].state, ControlActionState::Planned);
        assert!(registry.mark_stale_control_runs(true)?.is_empty());
        Ok(())
    }

    #[test]
    fn foreground_stale_marking_never_touches_worker_owned_runs() -> Result<()> {
        let (_temp, mut registry, project) = registry()?;
        let foreground = registry.plan_control_run(&input(&project, true))?;
        registry.begin_control_action(foreground.thread_action_id)?;

        let worker_run = registry.plan_control_run(&input(&project, true))?;
        let worker = registry.create_control_worker(worker_run.run_id)?;
        registry.mark_worker_ready(&worker, "127.0.0.1:12345", 42)?;

        let recovered = registry.mark_stale_control_runs(true)?;
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].id, foreground.run_id);
        assert_eq!(
            registry
                .control_run(worker_run.run_id)?
                .expect("worker run")
                .state,
            ControlRunState::Planned
        );
        assert_eq!(
            registry
                .control_worker(worker_run.run_id)?
                .expect("worker")
                .state,
            crate::WorkerState::Ready
        );
        Ok(())
    }

    #[test]
    fn recovery_covers_crashes_before_dispatch_and_between_actions() -> Result<()> {
        let (_temp, mut registry, project) = registry()?;
        let planned = registry.plan_control_run(&input(&project, true))?;
        assert!(matches!(
            registry.mark_stale_control_runs(false),
            Err(Error::InvalidControlRequest(_))
        ));
        let recovered = registry.mark_stale_control_runs(true)?;
        assert_eq!(recovered[0].id, planned.run_id);
        assert_eq!(recovered[0].state, ControlRunState::RecoveryRequired);
        assert!(matches!(
            registry.begin_control_action(planned.turn_action_id),
            Err(Error::ControlStateConflict(_))
        ));
        let detail = registry.control_run_detail(planned.run_id)?;
        assert_eq!(detail.actions[1].state, ControlActionState::Planned);
        let turn_state: String = registry.connection.query_row(
            "SELECT state FROM control_turns WHERE run_id = ?1",
            [planned.run_id],
            |row| row.get(0),
        )?;
        assert_eq!(turn_state, "planned");

        let between = registry.plan_control_run(&input(&project, true))?;
        registry.begin_control_action(between.thread_action_id)?;
        registry.acknowledge_thread_action(
            between.thread_action_id,
            "synthetic-thread",
            Some("synthetic-session"),
        )?;
        let recovered = registry.mark_stale_control_runs(true)?;
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].id, between.run_id);
        assert_eq!(recovered[0].state, ControlRunState::RecoveryRequired);
        Ok(())
    }

    #[test]
    fn dispatched_turn_is_never_left_planned_after_transport_loss() -> Result<()> {
        let (_temp, mut registry, project) = registry()?;
        let plan = registry.plan_control_run(&input(&project, true))?;
        registry.begin_control_action(plan.thread_action_id)?;
        registry.acknowledge_thread_action(
            plan.thread_action_id,
            "synthetic-thread",
            Some("synthetic-session"),
        )?;
        registry.begin_control_action(plan.turn_action_id)?;
        let state: String = registry.connection.query_row(
            "SELECT state FROM control_turns WHERE run_id = ?1",
            [plan.run_id],
            |row| row.get(0),
        )?;
        assert_eq!(state, "dispatching");
        registry.mark_control_uncertain(plan.run_id)?;
        let state: String = registry.connection.query_row(
            "SELECT state FROM control_turns WHERE run_id = ?1",
            [plan.run_id],
            |row| row.get(0),
        )?;
        assert_eq!(state, "uncertain");
        Ok(())
    }

    #[test]
    fn resume_requires_a_cataloged_thread_bound_to_the_project() -> Result<()> {
        let (_temp, mut registry, project) = registry()?;
        let mut resume = input(&project, true);
        resume.resume_thread_id = Some("synthetic-thread");
        assert!(matches!(
            registry.plan_control_run(&resume),
            Err(Error::InvalidControlRequest(_))
        ));
        catalog_thread(&mut registry, &project, "synthetic-thread")?;
        assert!(registry.plan_control_run(&resume).is_ok());
        Ok(())
    }

    #[test]
    fn recovery_marks_acknowledged_running_turn_uncertain_without_replay() -> Result<()> {
        let (_temp, mut registry, project) = registry()?;
        let plan = registry.plan_control_run(&input(&project, true))?;
        registry.begin_control_action(plan.thread_action_id)?;
        registry.acknowledge_thread_action(
            plan.thread_action_id,
            "synthetic-thread",
            Some("synthetic-session"),
        )?;
        registry.begin_control_action(plan.turn_action_id)?;
        registry.acknowledge_turn_action(plan.turn_action_id, "synthetic-turn")?;

        let recovered = registry.mark_stale_control_runs(true)?;
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].state, ControlRunState::RecoveryRequired);
        let detail = registry.control_run_detail(plan.run_id)?;
        assert_eq!(detail.actions[0].state, ControlActionState::Succeeded);
        assert_eq!(detail.actions[1].state, ControlActionState::Uncertain);
        assert!(registry.mark_stale_control_runs(true)?.is_empty());
        Ok(())
    }

    #[test]
    fn raw_failure_content_is_never_persisted() -> Result<()> {
        let (_temp, mut registry, project) = registry()?;
        let plan = registry.plan_control_run(&input(&project, true))?;
        registry.begin_control_action(plan.thread_action_id)?;
        registry.mark_control_uncertain(plan.run_id)?;
        let sentinel = "SENSITIVE_SENTINEL_MUST_NOT_APPEAR";
        let tables = [
            ("control_runs", "COALESCE(last_error_message, '')"),
            ("control_actions", "COALESCE(error_message, '')"),
            ("control_action_events", "COALESCE(detail, '')"),
        ];
        for (table, column) in tables {
            let query = format!("SELECT COUNT(*) FROM {table} WHERE {column} LIKE ?1");
            let count: i64 =
                registry
                    .connection
                    .query_row(&query, [format!("%{sentinel}%")], |row| row.get(0))?;
            assert_eq!(count, 0);
        }
        Ok(())
    }

    #[test]
    fn illegal_action_replay_is_rejected() -> Result<()> {
        let (_temp, mut registry, project) = registry()?;
        let plan = registry.plan_control_run(&input(&project, true))?;
        registry.begin_control_action(plan.thread_action_id)?;
        assert!(matches!(
            registry.begin_control_action(plan.thread_action_id),
            Err(Error::ControlStateConflict(_))
        ));
        Ok(())
    }

    #[test]
    fn owned_steer_is_idempotent_redacted_and_exact_turn_fenced() -> Result<()> {
        let (_temp, mut registry, project) = registry()?;
        let (run, claim) = active_worker_run(&mut registry, &project)?;
        let request_id = "00000000-0000-4000-8000-000000000123";
        let first = registry.plan_owned_steer(run.run_id, request_id, 27, &claim)?;
        assert!(first.should_dispatch);
        assert_eq!(first.turn_id, "synthetic-turn");
        let retry = registry.plan_owned_steer(run.run_id, request_id, 27, &claim)?;
        assert_eq!(retry.action_id, first.action_id);
        assert!(!retry.should_dispatch);
        assert!(matches!(
            registry.plan_owned_steer(run.run_id, request_id, 28, &claim),
            Err(Error::ControlStateConflict(_))
        ));
        registry.begin_owned_control_action(first.action_id, &claim)?;
        registry.acknowledge_owned_steer(first.action_id, "synthetic-turn", &claim)?;
        let detail = registry.control_run_detail(run.run_id)?;
        let steer = detail
            .actions
            .iter()
            .find(|action| action.action_kind == ControlActionKind::TurnSteer)
            .expect("steer action");
        assert_eq!(steer.state, ControlActionState::Succeeded);
        assert_eq!(steer.input_bytes, Some(27));
        assert_eq!(steer.client_request_id.as_deref(), Some(request_id));
        assert_eq!(
            steer.expected_source_turn_id.as_deref(),
            Some("synthetic-turn")
        );
        let serialized = serde_json::to_string(&detail)
            .map_err(|error| Error::InvalidControlRequest(error.to_string()))?;
        assert!(!serialized.contains("steer text"));
        Ok(())
    }

    #[test]
    fn interrupt_is_a_barrier_and_completion_fails_queued_steer() -> Result<()> {
        let (_temp, mut registry, project) = registry()?;
        let (run, claim) = active_worker_run(&mut registry, &project)?;
        let queued = registry.plan_owned_steer(
            run.run_id,
            "00000000-0000-4000-8000-000000000201",
            5,
            &claim,
        )?;
        let interrupt = registry.plan_owned_interrupt(run.run_id, &claim)?;
        assert!(interrupt.should_dispatch);
        assert!(matches!(
            registry.plan_owned_steer(
                run.run_id,
                "00000000-0000-4000-8000-000000000202",
                5,
                &claim
            ),
            Err(Error::ControlStateConflict(_))
        ));
        registry.complete_owned_control_run(run.run_id, "completed", &claim)?;
        let detail = registry.control_run_detail(run.run_id)?;
        let queued = detail
            .actions
            .iter()
            .find(|action| action.id == queued.action_id)
            .expect("queued steer");
        assert_eq!(queued.state, ControlActionState::Failed);
        Ok(())
    }

    #[test]
    fn terminal_reconciliation_preserves_uncertain_history() -> Result<()> {
        let (_temp, mut registry, project) = registry()?;
        let (run, claim) = active_worker_run(&mut registry, &project)?;
        let steer_id = "00000000-0000-4000-8000-000000000203";
        let steer = registry.plan_owned_steer(run.run_id, steer_id, 9, &claim)?;
        registry.begin_owned_control_action(steer.action_id, &claim)?;
        registry.connection.execute(
            "UPDATE control_workers SET lease_acquired_at = 0,
                lease_expires_at = 0, heartbeat_at = 0 WHERE run_id = ?1",
            [run.run_id],
        )?;
        assert_eq!(registry.recover_expired_workers()?, vec![run.run_id]);
        let reconcile =
            registry.plan_reconciliation(run.run_id, "00000000-0000-4000-8000-000000000204")?;
        registry.begin_reconciliation(reconcile.action_id)?;
        let observed = [steer_id];
        let resolved = registry.record_reconciliation(
            reconcile.action_id,
            &ReconciliationObservation {
                thread_id: "synthetic-thread",
                thread_status: "notLoaded",
                turn_id: Some("synthetic-turn"),
                turn_status: Some("completed"),
                initial_message_observed: true,
                observed_steer_client_ids: &observed,
            },
        )?;
        assert_eq!(resolved.state, ControlRunState::Completed);
        let detail = registry.control_run_detail(run.run_id)?;
        assert_eq!(
            detail
                .actions
                .iter()
                .find(|action| action.id == steer.action_id)
                .expect("steer")
                .state,
            ControlActionState::Uncertain
        );
        assert_eq!(
            detail.actions.last().expect("reconcile").state,
            ControlActionState::Succeeded
        );
        Ok(())
    }

    #[test]
    fn nonterminal_reconciliation_keeps_recovery_required() -> Result<()> {
        let (_temp, mut registry, project) = registry()?;
        let (run, _claim) = active_worker_run(&mut registry, &project)?;
        registry.connection.execute(
            "UPDATE control_workers SET lease_acquired_at = 0,
                lease_expires_at = 0, heartbeat_at = 0 WHERE run_id = ?1",
            [run.run_id],
        )?;
        registry.recover_expired_workers()?;
        let reconcile =
            registry.plan_reconciliation(run.run_id, "00000000-0000-4000-8000-000000000205")?;
        registry.begin_reconciliation(reconcile.action_id)?;
        let result = registry.record_reconciliation(
            reconcile.action_id,
            &ReconciliationObservation {
                thread_id: "synthetic-thread",
                thread_status: "notLoaded",
                turn_id: Some("synthetic-turn"),
                turn_status: Some("inProgress"),
                initial_message_observed: true,
                observed_steer_client_ids: &[],
            },
        )?;
        assert_eq!(result.state, ControlRunState::RecoveryRequired);
        Ok(())
    }

    #[test]
    fn reconciliation_maps_unknown_source_statuses_before_persistence() -> Result<()> {
        let (_temp, mut registry, project) = registry()?;
        let (run, claim) = active_worker_run(&mut registry, &project)?;
        registry.mark_owned_control_uncertain(run.run_id, &claim)?;
        registry.finish_worker(&claim, "clean")?;
        let reconcile =
            registry.plan_reconciliation(run.run_id, "00000000-0000-4000-8000-000000000206")?;
        registry.begin_reconciliation(reconcile.action_id)?;
        let sentinel = "PRIVATE_STATUS_SENTINEL";
        let result = registry.record_reconciliation(
            reconcile.action_id,
            &ReconciliationObservation {
                thread_id: "synthetic-thread",
                thread_status: sentinel,
                turn_id: Some("synthetic-turn"),
                turn_status: Some(sentinel),
                initial_message_observed: true,
                observed_steer_client_ids: &[],
            },
        )?;
        assert_eq!(result.state, ControlRunState::RecoveryRequired);
        let stored: (String, String, String) = registry.connection.query_row(
            "SELECT thread_status, observed_turn_status, outcome
             FROM control_reconciliations WHERE action_id = ?1",
            [reconcile.action_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(
            stored,
            (
                "unknown".to_owned(),
                "unknown".to_owned(),
                "unsupported_status".to_owned()
            )
        );
        let count: i64 = registry.connection.query_row(
            "SELECT COUNT(*) FROM control_reconciliations
             WHERE thread_status LIKE ?1 OR observed_turn_status LIKE ?1",
            [format!("%{sentinel}%")],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0);
        Ok(())
    }
}
