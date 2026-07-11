//! Fenced ownership for reconnectable per-run Codex workers.

use rusqlite::OptionalExtension;
use rusqlite::Transaction;
use rusqlite::params;
use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;

use crate::Error;
use crate::Registry;
use crate::Result;
use crate::registry::unix_timestamp;

pub(crate) const WORKER_LEASE_SECONDS: i64 = 10;

/// Durable lifecycle state for one per-run worker process.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    Starting,
    Ready,
    Busy,
    Stopping,
    Exited,
    Lost,
}

impl WorkerState {
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "starting" => Ok(Self::Starting),
            "ready" => Ok(Self::Ready),
            "busy" => Ok(Self::Busy),
            "stopping" => Ok(Self::Stopping),
            "exited" => Ok(Self::Exited),
            "lost" => Ok(Self::Lost),
            _ => Err(Error::ControlStateConflict(format!(
                "unknown worker state {value}"
            ))),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Busy => "busy",
            Self::Stopping => "stopping",
            Self::Exited => "exited",
            Self::Lost => "lost",
        }
    }
}

/// Redaction-safe durable worker projection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlWorker {
    pub id: i64,
    pub run_id: i64,
    pub state: WorkerState,
    pub protocol_version: u32,
    pub endpoint_ready: bool,
    pub pid: Option<u32>,
    pub process_started_at: i64,
    pub lease_epoch: i64,
    pub lease_expires_at: i64,
    pub heartbeat_at: i64,
    pub terminal_at: Option<i64>,
    pub exit_kind: Option<String>,
}

/// Secret-bearing ownership evidence used only inside the local worker protocol.
#[derive(Clone)]
pub struct WorkerClaim {
    pub(crate) id: i64,
    pub(crate) run_id: i64,
    pub(crate) run_key: String,
    pub(crate) worker_key: String,
    pub(crate) lease_epoch: i64,
}

impl WorkerClaim {
    #[must_use]
    pub const fn id(&self) -> i64 {
        self.id
    }

    #[must_use]
    pub const fn run_id(&self) -> i64 {
        self.run_id
    }

    #[must_use]
    pub fn run_key(&self) -> &str {
        &self.run_key
    }

    #[must_use]
    pub fn worker_key(&self) -> &str {
        &self.worker_key
    }

    #[must_use]
    pub const fn lease_epoch(&self) -> i64 {
        self.lease_epoch
    }
}

/// Authenticated local connection information. The key is deliberately not serializable.
pub struct WorkerConnection {
    pub run_id: i64,
    pub run_key: String,
    pub endpoint: String,
}

impl Registry {
    /// Allocate a single fenced worker identity for a planned run.
    pub fn create_control_worker(&mut self, run_id: i64) -> Result<WorkerClaim> {
        let now = unix_timestamp();
        let worker_key = Uuid::new_v4().to_string();
        let transaction = self.connection.transaction()?;
        let run_key: String = transaction
            .query_row(
                "SELECT run_key FROM control_runs WHERE id = ?1 AND state = 'planned'",
                [run_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                Error::ControlStateConflict(format!(
                    "run {run_id} was not planned for worker allocation"
                ))
            })?;
        transaction.execute(
            "INSERT INTO control_workers (
                worker_key, run_id, runtime_kind, protocol_version, process_started_at,
                state, lease_epoch, lease_acquired_at, lease_expires_at, heartbeat_at
             ) VALUES (?1, ?2, 'codex', 1, ?3, 'starting', 1, ?3, ?4, ?3)",
            params![worker_key, run_id, now, now + WORKER_LEASE_SECONDS],
        )?;
        let id = transaction.last_insert_rowid();
        let changed = transaction.execute(
            "UPDATE control_runs SET ownership_mode = 'worker', updated_at = ?1
             WHERE id = ?2 AND state = 'planned' AND ownership_mode = 'foreground'",
            params![now, run_id],
        )?;
        if changed != 1 {
            return Err(Error::ControlStateConflict(
                "run could not be promoted to worker ownership".to_owned(),
            ));
        }
        transaction.commit()?;
        Ok(WorkerClaim {
            id,
            run_id,
            run_key,
            worker_key,
            lease_epoch: 1,
        })
    }

    /// Load the private worker identity inside the spawned worker process.
    pub fn worker_claim(&self, run_id: i64) -> Result<WorkerClaim> {
        self.connection
            .query_row(
                "SELECT w.id, w.run_id, r.run_key, w.worker_key, w.lease_epoch
                 FROM control_workers w JOIN control_runs r ON r.id = w.run_id
                 WHERE w.run_id = ?1 AND w.state IN ('starting', 'ready', 'busy')",
                [run_id],
                |row| {
                    Ok(WorkerClaim {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        run_key: row.get(2)?,
                        worker_key: row.get(3)?,
                        lease_epoch: row.get(4)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| {
                Error::ControlStateConflict(format!("run {run_id} has no live worker claim"))
            })
    }

    /// Publish a loopback endpoint after the worker has bound it successfully.
    pub fn mark_worker_ready(
        &mut self,
        claim: &WorkerClaim,
        endpoint: &str,
        pid: u32,
    ) -> Result<ControlWorker> {
        if !endpoint.starts_with("127.0.0.1:") {
            return Err(Error::InvalidControlRequest(
                "worker endpoint must be IPv4 loopback".to_owned(),
            ));
        }
        let now = unix_timestamp();
        let changed = self.connection.execute(
            "UPDATE control_workers SET endpoint = ?1, pid = ?2, state = 'ready',
                heartbeat_at = ?3, lease_expires_at = ?4
             WHERE id = ?5 AND worker_key = ?6 AND lease_epoch = ?7
             AND state = 'starting' AND lease_expires_at >= ?3",
            params![
                endpoint,
                i64::from(pid),
                now,
                now + WORKER_LEASE_SECONDS,
                claim.id,
                claim.worker_key,
                claim.lease_epoch
            ],
        )?;
        if changed != 1 {
            return Err(Error::ControlStateConflict(
                "worker lost its starting lease before publishing its endpoint".to_owned(),
            ));
        }
        self.control_worker(claim.run_id)?
            .ok_or_else(|| Error::ControlStateConflict("ready worker disappeared".to_owned()))
    }

    /// Renew the exact fenced lease and optionally publish a new nonterminal state.
    pub fn heartbeat_worker(&mut self, claim: &WorkerClaim, state: WorkerState) -> Result<()> {
        if matches!(state, WorkerState::Exited | WorkerState::Lost) {
            return Err(Error::InvalidControlRequest(
                "heartbeat cannot publish a terminal worker state".to_owned(),
            ));
        }
        let now = unix_timestamp();
        let changed = self.connection.execute(
            "UPDATE control_workers SET state = ?1, heartbeat_at = ?2, lease_expires_at = ?3
             WHERE id = ?4 AND worker_key = ?5 AND lease_epoch = ?6
             AND state IN ('starting', 'ready', 'busy', 'stopping')
             AND lease_expires_at >= ?2",
            params![
                state.as_str(),
                now,
                now + WORKER_LEASE_SECONDS,
                claim.id,
                claim.worker_key,
                claim.lease_epoch
            ],
        )?;
        if changed != 1 {
            return Err(Error::ControlStateConflict(
                "worker heartbeat lost its lease fence".to_owned(),
            ));
        }
        Ok(())
    }

    /// Mark a worker terminal under its current fence.
    pub fn finish_worker(&mut self, claim: &WorkerClaim, exit_kind: &str) -> Result<()> {
        let now = unix_timestamp();
        let changed = self.connection.execute(
            "UPDATE control_workers SET state = 'exited', terminal_at = ?1,
                exit_kind = ?2, heartbeat_at = ?1
             WHERE id = ?3 AND worker_key = ?4 AND lease_epoch = ?5
             AND state IN ('starting', 'ready', 'busy', 'stopping')
             AND lease_expires_at >= ?1",
            params![
                now,
                exit_kind,
                claim.id,
                claim.worker_key,
                claim.lease_epoch
            ],
        )?;
        if changed != 1 {
            return Err(Error::ControlStateConflict(
                "worker could not finish under its lease fence".to_owned(),
            ));
        }
        Ok(())
    }

    /// Fail a worker allocation only when no external control mutation was dispatched.
    pub fn fail_worker_without_submission(&mut self, claim: &WorkerClaim) -> Result<()> {
        let now = unix_timestamp();
        let transaction = self.connection.transaction()?;
        assert_worker_fence(&transaction, claim, &["starting", "ready"])?;
        let dispatched: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM control_actions WHERE run_id = ?1 AND state != 'planned'",
            [claim.run_id],
            |row| row.get(0),
        )?;
        if dispatched != 0 {
            return Err(Error::ControlStateConflict(
                "worker submission failure was no longer pre-dispatch".to_owned(),
            ));
        }
        transaction.execute(
            "UPDATE control_actions SET state = 'failed', terminal_at = ?1,
                error_class = 'worker_start_failed',
                error_message = 'worker stopped before prompt submission'
             WHERE run_id = ?2 AND state = 'planned'",
            params![now, claim.run_id],
        )?;
        transaction.execute(
            "INSERT INTO control_action_events (action_id, sequence, event_kind, recorded_at, detail)
             SELECT a.id,
                COALESCE((SELECT MAX(e.sequence) FROM control_action_events e
                          WHERE e.action_id = a.id), 0) + 1,
                'failed', ?1, NULL
             FROM control_actions a
             WHERE a.run_id = ?2 AND a.error_class = 'worker_start_failed'",
            params![now, claim.run_id],
        )?;
        transaction.execute(
            "UPDATE control_turns SET state = 'failed', terminal_at = ?1
             WHERE run_id = ?2 AND state = 'planned'",
            params![now, claim.run_id],
        )?;
        let run_changed = transaction.execute(
            "UPDATE control_runs SET state = 'failed', terminal_at = ?1, updated_at = ?1,
                last_error_class = 'worker_start_failed',
                last_error_message = 'worker stopped before prompt submission'
             WHERE id = ?2 AND state = 'planned' AND ownership_mode = 'worker'",
            params![now, claim.run_id],
        )?;
        let worker_changed = transaction.execute(
            "UPDATE control_workers SET state = 'exited', terminal_at = ?1,
                heartbeat_at = ?1, endpoint = NULL, exit_kind = 'worker_error',
                last_error_class = 'worker_start_failed',
                last_error_message = 'worker stopped before prompt submission'
             WHERE id = ?2 AND worker_key = ?3 AND lease_epoch = ?4
             AND state IN ('starting', 'ready') AND lease_expires_at >= ?1",
            params![now, claim.id, claim.worker_key, claim.lease_epoch],
        )?;
        if run_changed != 1 || worker_changed != 1 {
            return Err(Error::ControlStateConflict(
                "pre-submission failure transition lost its fence".to_owned(),
            ));
        }
        transaction.commit()?;
        Ok(())
    }

    /// Atomically quarantine every expired worker without replaying any mutation.
    pub fn recover_expired_workers(&mut self) -> Result<Vec<i64>> {
        let now = unix_timestamp();
        let transaction = self.connection.transaction()?;
        let expired = {
            let mut statement = transaction.prepare(
                "SELECT id, run_id, lease_epoch FROM control_workers
                 WHERE state IN ('starting', 'ready', 'busy', 'stopping')
                 AND lease_expires_at < ?1",
            )?;
            statement
                .query_map([now], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut recovered = Vec::new();
        for (worker_id, run_id, epoch) in expired {
            let worker_changed = transaction.execute(
                "UPDATE control_workers SET state = 'lost', terminal_at = ?1,
                    heartbeat_at = ?1, endpoint = NULL, lease_epoch = lease_epoch + 1,
                    exit_kind = 'lease_lost', last_error_class = 'worker_lost',
                    last_error_message = 'worker heartbeat expired; source reconciliation is required'
                 WHERE id = ?2 AND run_id = ?3 AND lease_epoch = ?4
                 AND state IN ('starting', 'ready', 'busy', 'stopping')
                 AND lease_expires_at < ?1",
                params![now, worker_id, run_id, epoch],
            )?;
            if worker_changed != 1 {
                continue;
            }
            let action_ids = {
                let mut statement = transaction.prepare(
                    "SELECT id FROM control_actions WHERE run_id = ?1 AND worker_id = ?2
                     AND worker_lease_epoch = ?3 AND state IN ('dispatching', 'acknowledged')",
                )?;
                statement
                    .query_map(params![run_id, worker_id, epoch], |row| {
                        row.get::<_, i64>(0)
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            for action_id in action_ids {
                transaction.execute(
                    "UPDATE control_actions SET state = 'uncertain', terminal_at = ?1,
                        error_class = 'worker_lost',
                        error_message = 'worker heartbeat expired after dispatch'
                     WHERE id = ?2",
                    params![now, action_id],
                )?;
                let sequence: i64 = transaction.query_row(
                    "SELECT COALESCE(MAX(sequence), 0) + 1
                     FROM control_action_events WHERE action_id = ?1",
                    [action_id],
                    |row| row.get(0),
                )?;
                transaction.execute(
                    "INSERT INTO control_action_events (
                        action_id, sequence, event_kind, recorded_at, detail
                     ) VALUES (?1, ?2, 'uncertain', ?3, NULL)",
                    params![action_id, sequence, now],
                )?;
            }
            transaction.execute(
                "UPDATE control_turns SET state = 'uncertain', terminal_at = ?1
                 WHERE run_id = ?2 AND state IN ('dispatching', 'running')",
                params![now, run_id],
            )?;
            transaction.execute(
                "UPDATE control_runs SET state = 'recovery_required', updated_at = ?1,
                    last_error_class = 'worker_lost',
                    last_error_message = 'read-only reconciliation is required before retry'
                 WHERE id = ?2 AND state IN ('planned', 'starting', 'active')",
                params![now, run_id],
            )?;
            recovered.push(run_id);
        }
        transaction.commit()?;
        Ok(recovered)
    }

    /// Return the redaction-safe durable worker projection for a run.
    pub fn control_worker(&self, run_id: i64) -> Result<Option<ControlWorker>> {
        self.connection
            .query_row(
                "SELECT id, run_id, state, protocol_version, endpoint IS NOT NULL, pid,
                    process_started_at, lease_epoch, lease_expires_at, heartbeat_at,
                    terminal_at, exit_kind
                 FROM control_workers WHERE run_id = ?1",
                [run_id],
                worker_from_row,
            )
            .optional()
            .map_err(Error::from)
    }

    /// Resolve private connection data only while the exact worker lease is current.
    pub fn worker_connection(&self, run_id: i64) -> Result<WorkerConnection> {
        let now = unix_timestamp();
        self.connection
            .query_row(
                "SELECT w.run_id, r.run_key, w.endpoint
                 FROM control_workers w JOIN control_runs r ON r.id = w.run_id
                 WHERE w.run_id = ?1 AND w.state IN ('ready', 'busy')
                 AND w.lease_expires_at >= ?2 AND w.endpoint IS NOT NULL",
                params![run_id, now],
                |row| {
                    Ok(WorkerConnection {
                        run_id: row.get(0)?,
                        run_key: row.get(1)?,
                        endpoint: row.get(2)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| {
                Error::ControlStateConflict(format!(
                    "run {run_id} has no connectable worker with a current lease"
                ))
            })
    }
}

pub(crate) fn assert_worker_fence(
    transaction: &Transaction<'_>,
    claim: &WorkerClaim,
    allowed_states: &[&str],
) -> Result<()> {
    let now = unix_timestamp();
    let placeholders = std::iter::repeat_n("?", allowed_states.len())
        .collect::<Vec<_>>()
        .join(",");
    let query = format!(
        "SELECT COUNT(*) FROM control_workers
         WHERE id = ?1 AND run_id = ?2 AND worker_key = ?3 AND lease_epoch = ?4
         AND lease_expires_at >= ?5 AND state IN ({placeholders})"
    );
    let mut values: Vec<rusqlite::types::Value> = vec![
        claim.id.into(),
        claim.run_id.into(),
        claim.worker_key.clone().into(),
        claim.lease_epoch.into(),
        now.into(),
    ];
    values.extend(
        allowed_states
            .iter()
            .map(|value| (*value).to_owned().into()),
    );
    let count: i64 =
        transaction.query_row(&query, rusqlite::params_from_iter(values), |row| row.get(0))?;
    if count != 1 {
        return Err(Error::ControlStateConflict(
            "worker lease fence is no longer current".to_owned(),
        ));
    }
    Ok(())
}

fn worker_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ControlWorker> {
    let state = row.get::<_, String>(2)?;
    Ok(ControlWorker {
        id: row.get(0)?,
        run_id: row.get(1)?,
        state: WorkerState::from_str(&state).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        protocol_version: row.get(3)?,
        endpoint_ready: row.get::<_, i64>(4)? != 0,
        pid: row.get(5)?,
        process_started_at: row.get(6)?,
        lease_epoch: row.get(7)?,
        lease_expires_at: row.get(8)?,
        heartbeat_at: row.get(9)?,
        terminal_at: row.get(10)?,
        exit_kind: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use crate::NewControlRun;
    use crate::SkeinPaths;

    use super::*;

    #[test]
    fn worker_lease_is_singleton_private_and_fenced() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary worker test"),
            source,
        })?;
        let project = temp.path().join("project");
        fs::create_dir(&project).map_err(|source| Error::Io {
            path: project.clone(),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let mut registry = Registry::open(&paths)?;
        registry.add_project(&project, None)?;
        let plan = registry.plan_control_run(&NewControlRun {
            project_path: &project,
            resume_thread_id: None,
            prompt: "Synthetic prompt",
            full_access_acknowledged: true,
        })?;
        let claim = registry.create_control_worker(plan.run_id)?;
        assert!(registry.create_control_worker(plan.run_id).is_err());
        let worker = registry.mark_worker_ready(&claim, "127.0.0.1:12345", 42)?;
        assert_eq!(worker.state, WorkerState::Ready);
        registry.heartbeat_worker(&claim, WorkerState::Busy)?;
        let connection = registry.worker_connection(plan.run_id)?;
        assert_eq!(connection.run_key, claim.run_key());
        registry.finish_worker(&claim, "clean")?;
        assert!(registry.worker_connection(plan.run_id).is_err());
        Ok(())
    }

    #[test]
    fn stale_worker_epoch_cannot_acknowledge_an_owned_action() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary worker fence test"),
            source,
        })?;
        let project = temp.path().join("project");
        fs::create_dir(&project).map_err(|source| Error::Io {
            path: project.clone(),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let mut registry = Registry::open(&paths)?;
        registry.add_project(&project, None)?;
        let plan = registry.plan_control_run(&NewControlRun {
            project_path: &project,
            resume_thread_id: None,
            prompt: "Synthetic prompt",
            full_access_acknowledged: true,
        })?;
        let claim = registry.create_control_worker(plan.run_id)?;
        registry.mark_worker_ready(&claim, "127.0.0.1:12345", 42)?;
        registry.heartbeat_worker(&claim, WorkerState::Busy)?;
        registry.begin_owned_control_action(plan.thread_action_id, &claim)?;

        let stale = WorkerClaim {
            lease_epoch: claim.lease_epoch + 1,
            ..claim.clone()
        };
        assert!(matches!(
            registry.acknowledge_owned_thread_action(
                plan.thread_action_id,
                "synthetic-thread",
                Some("synthetic-session"),
                &stale
            ),
            Err(Error::ControlStateConflict(_))
        ));
        registry.acknowledge_owned_thread_action(
            plan.thread_action_id,
            "synthetic-thread",
            Some("synthetic-session"),
            &claim,
        )?;
        let owner: (i64, i64) = registry.connection.query_row(
            "SELECT worker_id, worker_lease_epoch FROM control_actions WHERE id = ?1",
            [plan.thread_action_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(owner, (claim.id, claim.lease_epoch));

        registry.begin_owned_control_action(plan.turn_action_id, &claim)?;
        registry.acknowledge_owned_turn_action(plan.turn_action_id, "synthetic-turn", &claim)?;
        let interrupt = registry.plan_owned_interrupt(plan.run_id, &claim)?;
        registry.begin_owned_control_action(interrupt.action_id, &claim)?;
        registry.acknowledge_owned_interrupt(interrupt.action_id, "synthetic-turn", &claim)?;
        registry.complete_owned_control_run(plan.run_id, "completed", &claim)?;
        let interrupt_result: (String, Option<String>) = registry.connection.query_row(
            "SELECT state, error_class FROM control_actions WHERE id = ?1",
            [interrupt.action_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(interrupt_result.0, "failed");
        assert_eq!(interrupt_result.1.as_deref(), Some("interrupt_raced"));
        Ok(())
    }

    #[test]
    fn expired_worker_is_fenced_and_quarantined_without_replay() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary expired worker test"),
            source,
        })?;
        let project = temp.path().join("project");
        fs::create_dir(&project).map_err(|source| Error::Io {
            path: project.clone(),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let mut registry = Registry::open(&paths)?;
        registry.add_project(&project, None)?;
        let plan = registry.plan_control_run(&NewControlRun {
            project_path: &project,
            resume_thread_id: None,
            prompt: "Synthetic prompt",
            full_access_acknowledged: true,
        })?;
        let claim = registry.create_control_worker(plan.run_id)?;
        registry.mark_worker_ready(&claim, "127.0.0.1:12345", 42)?;
        registry.heartbeat_worker(&claim, WorkerState::Busy)?;
        registry.begin_owned_control_action(plan.thread_action_id, &claim)?;
        registry.connection.execute(
            "UPDATE control_workers SET lease_acquired_at = 0, heartbeat_at = 0,
                lease_expires_at = 0 WHERE id = ?1",
            [claim.id],
        )?;

        assert!(
            registry
                .heartbeat_worker(&claim, WorkerState::Busy)
                .is_err()
        );
        assert_eq!(registry.recover_expired_workers()?, vec![plan.run_id]);
        assert_eq!(
            registry.control_worker(plan.run_id)?.expect("worker").state,
            WorkerState::Lost
        );
        assert_eq!(
            registry.control_run(plan.run_id)?.expect("run").state,
            crate::ControlRunState::RecoveryRequired
        );
        assert!(
            registry
                .begin_owned_control_action(plan.turn_action_id, &claim)
                .is_err()
        );
        assert!(registry.recover_expired_workers()?.is_empty());
        Ok(())
    }
}
