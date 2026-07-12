//! Durable, redaction-safe monitoring projections for reconnectable runs.

use rusqlite::OptionalExtension;
use rusqlite::params;
use serde::Serialize;

use crate::ControlRun;
use crate::ControlRunState;
use crate::ControlWorker;
use crate::Error;
use crate::Registry;
use crate::Result;
use crate::WorkerState;

pub const MAX_MONITOR_EVENTS: usize = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationPhase {
    NotRequested,
    RequestedQueued,
    ObservedDispatching,
    Acknowledged,
    RequestFailed,
    RequestUncertain,
    TerminalInterrupted,
    TerminalCompleted,
    TerminalFailed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancellationStatus {
    pub phase: CancellationPhase,
    pub action_id: Option<i64>,
    pub requested: bool,
    pub observed_by_worker: bool,
    pub acknowledged_by_protocol: bool,
    pub terminal: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableMonitorEvent {
    pub cursor: i64,
    pub action_id: i64,
    pub action_kind: String,
    pub event_kind: String,
    pub recorded_at: i64,
    pub content_redacted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerHealth {
    pub state: WorkerState,
    pub heartbeat_age_seconds: i64,
    pub lease_remaining_seconds: i64,
    pub lease_expired: bool,
    pub stale: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunObservation {
    pub run: ControlRun,
    pub worker: Option<ControlWorker>,
    pub worker_health: Option<WorkerHealth>,
    pub cancellation: CancellationStatus,
    pub events: Vec<DurableMonitorEvent>,
    pub requested_after: i64,
    pub next_cursor: i64,
    pub has_more_events: bool,
    pub pending_actions: usize,
    pub last_durable_event: Option<DurableMonitorEvent>,
    pub terminal: bool,
    pub recovery_required: bool,
    pub recommended_next_action: String,
    pub checked_at: i64,
    pub content_redacted: bool,
}

impl Registry {
    pub fn observe_run(
        &self,
        run_id: i64,
        after_cursor: i64,
        limit: usize,
        now: i64,
    ) -> Result<RunObservation> {
        if after_cursor < 0 {
            return Err(Error::InvalidControlRequest(
                "monitor cursor must be non-negative".to_owned(),
            ));
        }
        let limit = limit.clamp(1, MAX_MONITOR_EVENTS);
        let run = self
            .control_run(run_id)?
            .ok_or_else(|| Error::InvalidControlRequest(format!("run {run_id} was not found")))?;
        let worker = self.control_worker(run_id)?;
        let mut statement = self.connection.prepare(
            "SELECT e.id, e.action_id, a.action_kind, e.event_kind, e.recorded_at
               FROM control_action_events e
               JOIN control_actions a ON a.id = e.action_id
              WHERE a.run_id = ?1 AND e.id > ?2
              ORDER BY e.id LIMIT ?3",
        )?;
        let rows = statement
            .query_map(
                params![
                    run_id,
                    after_cursor,
                    i64::try_from(limit + 1).unwrap_or(i64::MAX)
                ],
                |row| {
                    Ok(DurableMonitorEvent {
                        cursor: row.get(0)?,
                        action_id: row.get(1)?,
                        action_kind: row.get(2)?,
                        event_kind: row.get(3)?,
                        recorded_at: row.get(4)?,
                        content_redacted: true,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more_events = rows.len() > limit;
        let events = rows.into_iter().take(limit).collect::<Vec<_>>();
        let next_cursor = events.last().map_or(after_cursor, |event| event.cursor);
        let last_durable_event = self
            .connection
            .query_row(
                "SELECT e.id, e.action_id, a.action_kind, e.event_kind, e.recorded_at
                   FROM control_action_events e JOIN control_actions a ON a.id = e.action_id
                  WHERE a.run_id = ?1 ORDER BY e.id DESC LIMIT 1",
                [run_id],
                |row| {
                    Ok(DurableMonitorEvent {
                        cursor: row.get(0)?,
                        action_id: row.get(1)?,
                        action_kind: row.get(2)?,
                        event_kind: row.get(3)?,
                        recorded_at: row.get(4)?,
                        content_redacted: true,
                    })
                },
            )
            .optional()?;
        let pending: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM control_actions
              WHERE run_id = ?1 AND state IN ('planned', 'dispatching', 'acknowledged')",
            [run_id],
            |row| row.get(0),
        )?;
        let interrupt = self
            .connection
            .query_row(
                "SELECT id, state FROM control_actions
                  WHERE run_id = ?1 AND action_kind = 'turn_interrupt'",
                [run_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let cancellation = cancellation_status(run.state, interrupt.as_ref());
        let terminal = matches!(
            run.state,
            ControlRunState::Completed | ControlRunState::Failed | ControlRunState::Interrupted
        );
        let recovery_required = run.state == ControlRunState::RecoveryRequired;
        let worker_health = worker.as_ref().map(|item| WorkerHealth {
            state: item.state,
            heartbeat_age_seconds: now.saturating_sub(item.heartbeat_at).max(0),
            lease_remaining_seconds: item.lease_expires_at.saturating_sub(now),
            lease_expired: item.lease_expires_at < now,
            stale: matches!(item.state, WorkerState::Lost) || item.lease_expires_at < now,
        });
        let recommended_next_action = if terminal {
            "none_terminal"
        } else if recovery_required || worker_health.as_ref().is_some_and(|health| health.stale) {
            "reconcile"
        } else if has_more_events {
            "observe_next_cursor"
        } else if cancellation.requested && !cancellation.terminal {
            "wait_for_interrupt_outcome"
        } else {
            "wait"
        }
        .to_owned();
        Ok(RunObservation {
            run,
            worker,
            worker_health,
            cancellation,
            events,
            requested_after: after_cursor,
            next_cursor,
            has_more_events,
            pending_actions: usize::try_from(pending).unwrap_or(usize::MAX),
            last_durable_event,
            terminal,
            recovery_required,
            recommended_next_action,
            checked_at: now,
            content_redacted: true,
        })
    }
}

fn cancellation_status(
    run_state: ControlRunState,
    interrupt: Option<&(i64, String)>,
) -> CancellationStatus {
    let terminal_phase = match run_state {
        ControlRunState::Interrupted => Some(CancellationPhase::TerminalInterrupted),
        ControlRunState::Completed => Some(CancellationPhase::TerminalCompleted),
        ControlRunState::Failed => Some(CancellationPhase::TerminalFailed),
        _ => None,
    };
    let phase =
        terminal_phase.unwrap_or_else(|| match interrupt.map(|(_, state)| state.as_str()) {
            None => CancellationPhase::NotRequested,
            Some("planned") => CancellationPhase::RequestedQueued,
            Some("dispatching") => CancellationPhase::ObservedDispatching,
            Some("acknowledged" | "succeeded") => CancellationPhase::Acknowledged,
            Some("failed") => CancellationPhase::RequestFailed,
            Some("uncertain") => CancellationPhase::RequestUncertain,
            Some(_) => CancellationPhase::RequestUncertain,
        });
    CancellationStatus {
        phase,
        action_id: interrupt.map(|(id, _)| *id),
        requested: interrupt.is_some(),
        observed_by_worker: interrupt.is_some_and(|(_, state)| state != "planned"),
        acknowledged_by_protocol: interrupt
            .is_some_and(|(_, state)| matches!(state.as_str(), "acknowledged" | "succeeded")),
        terminal: terminal_phase.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::NewControlRun;
    use crate::SkeinPaths;

    fn active_worker() -> Result<(tempfile::TempDir, Registry, crate::WorkerClaim, i64)> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("synthetic monitor state"),
            source,
        })?;
        let project = temp.path().join("project");
        fs::create_dir(&project).map_err(|source| Error::Io {
            path: project.clone(),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let mut registry = Registry::open(&paths)?;
        registry.add_project(&project, Some("Synthetic Monitor"))?;
        let plan = registry.plan_control_run(&NewControlRun {
            project_path: &project,
            resume_thread_id: None,
            prompt: "private synthetic prompt",
            full_access_acknowledged: true,
        })?;
        let claim = registry.create_control_worker(plan.run_id)?;
        registry.mark_worker_ready(&claim, "127.0.0.1:12345", 42)?;
        registry.heartbeat_worker(&claim, WorkerState::Busy)?;
        registry.begin_owned_control_action(plan.thread_action_id, &claim)?;
        registry.acknowledge_owned_thread_action(
            plan.thread_action_id,
            "synthetic-thread",
            Some("synthetic-session"),
            &claim,
        )?;
        registry.begin_owned_control_action(plan.turn_action_id, &claim)?;
        registry.acknowledge_owned_turn_action(plan.turn_action_id, "synthetic-turn", &claim)?;
        Ok((temp, registry, claim, plan.run_id))
    }

    #[test]
    fn interrupt_phases_are_idempotent_and_terminal_truthful() -> Result<()> {
        let (_temp, mut registry, claim, run_id) = active_worker()?;
        let first = registry.plan_owned_interrupt(run_id, &claim)?;
        let repeated = registry.plan_owned_interrupt(run_id, &claim)?;
        assert_eq!(first.action_id, repeated.action_id);
        assert_eq!(
            registry
                .observe_run(run_id, 0, 100, 100)?
                .cancellation
                .phase,
            CancellationPhase::RequestedQueued
        );
        registry.begin_owned_control_action(first.action_id, &claim)?;
        assert_eq!(
            registry
                .observe_run(run_id, 0, 100, 100)?
                .cancellation
                .phase,
            CancellationPhase::ObservedDispatching
        );
        registry.acknowledge_owned_interrupt(first.action_id, "synthetic-turn", &claim)?;
        let acknowledged = registry.observe_run(run_id, 0, 100, 100)?;
        assert_eq!(
            acknowledged.cancellation.phase,
            CancellationPhase::Acknowledged
        );
        assert!(!acknowledged.terminal);
        registry.complete_owned_control_run(run_id, "interrupted", &claim)?;
        let terminal = registry.observe_run(run_id, acknowledged.next_cursor, 100, 100)?;
        assert_eq!(
            terminal.cancellation.phase,
            CancellationPhase::TerminalInterrupted
        );
        assert!(terminal.terminal);
        assert_eq!(terminal.recommended_next_action, "none_terminal");
        Ok(())
    }

    #[test]
    fn durable_cursor_pages_without_duplicates_and_reports_stale_health() -> Result<()> {
        let (_temp, registry, claim, run_id) = active_worker()?;
        registry.connection.execute(
            "UPDATE control_workers SET lease_acquired_at = 0, heartbeat_at = 10,
                    lease_expires_at = 20 WHERE id = ?1",
            [claim.id],
        )?;
        let mut cursor = 0;
        let mut seen = std::collections::BTreeSet::new();
        loop {
            let page = registry.observe_run(run_id, cursor, 1, 100)?;
            for event in &page.events {
                assert!(seen.insert(event.cursor));
                assert!(event.content_redacted);
                assert!(!format!("{event:?}").contains("private synthetic prompt"));
            }
            cursor = page.next_cursor;
            if !page.has_more_events {
                assert!(
                    page.worker_health
                        .as_ref()
                        .is_some_and(|health| health.stale)
                );
                assert_eq!(page.recommended_next_action, "reconcile");
                break;
            }
        }
        assert!(!seen.is_empty());
        Ok(())
    }
}
