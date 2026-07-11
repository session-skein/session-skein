//! Reconnectable per-run worker runtime and private loopback protocol.

use std::collections::VecDeque;
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::io::Write;
use std::net::Shutdown;
use std::net::TcpListener;
use std::net::TcpStream;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use skein_codex::ControlClient;
use skein_codex::ControlEvent;
use skein_core::ControlRun;
use skein_core::ControlRunState;
use skein_core::ControlWorker;
use skein_core::InterruptPlan;
use skein_core::ReconciliationObservation;
use skein_core::Registry;
use skein_core::SkeinPaths;
use skein_core::SteerPlan;
use skein_core::WorkerClaim;
use skein_core::WorkerState;
use uuid::Uuid;

const PROTOCOL_VERSION: u32 = 1;
const MAX_IPC_REQUEST_BYTES: usize = 1024 * 1024 + 4096;
const MAX_EVENTS: usize = 512;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const READY_TIMEOUT: Duration = Duration::from_secs(10);
const SUBMISSION_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const TERMINAL_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WorkerRequest {
    Submit {
        protocol_version: u32,
        run_key: String,
        capability: String,
        prompt: String,
    },
    Snapshot {
        protocol_version: u32,
        run_key: String,
        capability: String,
        after_sequence: u64,
    },
    Shutdown {
        protocol_version: u32,
        run_key: String,
        capability: String,
    },
    Interrupt {
        protocol_version: u32,
        run_key: String,
        capability: String,
    },
    Steer {
        protocol_version: u32,
        run_key: String,
        capability: String,
        client_request_id: String,
        prompt: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WorkerResponse {
    Accepted {
        run_id: i64,
    },
    Stopped {
        run_id: i64,
    },
    InterruptAccepted {
        run_id: i64,
    },
    SteerAccepted {
        run_id: i64,
        action_id: i64,
        queued: bool,
    },
    Snapshot(Box<WorkerSnapshot>),
    Error {
        code: String,
        message: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkerSnapshot {
    pub run: ControlRun,
    pub worker: ControlWorker,
    pub events: Vec<RedactedWorkerEvent>,
    pub latest_sequence: u64,
    pub oldest_sequence: Option<u64>,
    pub events_truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RedactedWorkerEvent {
    pub sequence: u64,
    pub kind: String,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub status: Option<String>,
    pub item_type: Option<String>,
    pub delta_bytes: Option<usize>,
}

#[derive(Default)]
struct SharedState {
    submitted: AtomicBool,
    shutdown: AtomicBool,
    interrupt_queued: AtomicBool,
    commands: Mutex<VecDeque<RuntimeCommand>>,
    events: Mutex<VecDeque<RedactedWorkerEvent>>,
    next_sequence: Mutex<u64>,
}

enum RuntimeCommand {
    Interrupt(InterruptPlan),
    Steer { plan: SteerPlan, prompt: String },
}

impl SharedState {
    fn push(&self, event: &ControlEvent) {
        let mut sequence = self
            .next_sequence
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *sequence += 1;
        let redacted = redact_event(*sequence, event);
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if events.len() == MAX_EVENTS {
            events.pop_front();
        }
        events.push_back(redacted);
    }

    fn snapshot(&self, after_sequence: u64) -> (Vec<RedactedWorkerEvent>, u64, Option<u64>, bool) {
        let latest = *self
            .next_sequence
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let guard = self
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let oldest = guard.front().map(|event| event.sequence);
        let truncated = oldest.is_some_and(|oldest| after_sequence.saturating_add(1) < oldest);
        let events = guard
            .iter()
            .filter(|event| event.sequence > after_sequence)
            .cloned()
            .collect();
        (events, latest, oldest, truncated)
    }
}

pub fn launch_worker(
    paths: &SkeinPaths,
    registry: &mut Registry,
    run_id: i64,
    prompt: String,
) -> Result<WorkerSnapshot, Box<dyn std::error::Error>> {
    let claim = registry.create_control_worker(run_id)?;
    launch_worker_with_claim(paths, registry, claim, prompt)
}

pub fn launch_preallocated_worker(
    paths: &SkeinPaths,
    registry: &mut Registry,
    claim: WorkerClaim,
    prompt: String,
) -> Result<WorkerSnapshot, Box<dyn std::error::Error>> {
    launch_worker_with_claim(paths, registry, claim, prompt)
}

fn launch_worker_with_claim(
    paths: &SkeinPaths,
    registry: &mut Registry,
    claim: WorkerClaim,
    prompt: String,
) -> Result<WorkerSnapshot, Box<dyn std::error::Error>> {
    let run_id = claim.run_id();
    if cfg!(debug_assertions)
        && std::env::var_os("SKEIN_TEST_FAIL_WORKER_LAUNCH").as_deref()
            == Some(std::ffi::OsStr::new("1"))
    {
        registry.fail_worker_without_submission(&claim)?;
        return Err("injected worker launch failure".into());
    }
    let executable = std::env::current_exe()?;
    let mut worker = Command::new(executable);
    worker
        .args(["worker", "serve", &run_id.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_detached_worker(&mut worker);
    let spawned = worker.spawn();
    if let Err(error) = spawned {
        registry.fail_worker_without_submission(&claim)?;
        return Err(error.into());
    }
    let connection = match await_connection(paths, run_id) {
        Ok(connection) => connection,
        Err(error) => {
            let _ = registry.fail_worker_without_submission(&claim);
            remove_capability(paths, run_id);
            return Err(error);
        }
    };
    let response = request(
        &connection.endpoint,
        &WorkerRequest::Submit {
            protocol_version: PROTOCOL_VERSION,
            run_key: connection.run_key.clone(),
            capability: read_capability(paths, run_id)?,
            prompt,
        },
    )?;
    if !matches!(response, WorkerResponse::Accepted { run_id: accepted } if accepted == run_id) {
        return Err("worker did not accept the planned run".into());
    }
    snapshot(paths, run_id, 0)
}

pub fn snapshot(
    paths: &SkeinPaths,
    run_id: i64,
    after_sequence: u64,
) -> Result<WorkerSnapshot, Box<dyn std::error::Error>> {
    let mut registry = Registry::open(paths)?;
    recover_expired(&mut registry, paths)?;
    let connection = registry.worker_connection(run_id)?;
    match request(
        &connection.endpoint,
        &WorkerRequest::Snapshot {
            protocol_version: PROTOCOL_VERSION,
            run_key: connection.run_key.clone(),
            capability: read_capability(paths, run_id)?,
            after_sequence,
        },
    )? {
        WorkerResponse::Snapshot(snapshot) => Ok(*snapshot),
        WorkerResponse::Error { code, message } => Err(format!("{code}: {message}").into()),
        WorkerResponse::Accepted { .. }
        | WorkerResponse::Stopped { .. }
        | WorkerResponse::InterruptAccepted { .. }
        | WorkerResponse::SteerAccepted { .. } => {
            Err("worker returned an unexpected response".into())
        }
    }
}

pub fn stop(paths: &SkeinPaths, run_id: i64) -> Result<(), Box<dyn std::error::Error>> {
    let mut registry = Registry::open(paths)?;
    recover_expired(&mut registry, paths)?;
    let connection = registry.worker_connection(run_id)?;
    match request(
        &connection.endpoint,
        &WorkerRequest::Shutdown {
            protocol_version: PROTOCOL_VERSION,
            run_key: connection.run_key.clone(),
            capability: read_capability(paths, run_id)?,
        },
    )? {
        WorkerResponse::Stopped { run_id: stopped } if stopped == run_id => Ok(()),
        WorkerResponse::Error { code, message } => Err(format!("{code}: {message}").into()),
        _ => Err("worker returned an unexpected stop response".into()),
    }
}

pub fn interrupt(paths: &SkeinPaths, run_id: i64) -> Result<(), Box<dyn std::error::Error>> {
    let mut registry = Registry::open(paths)?;
    recover_expired(&mut registry, paths)?;
    if let Some(run) = registry.control_run(run_id)?
        && terminal(run.state)
    {
        return Ok(());
    }
    let connection = registry.worker_connection(run_id)?;
    match request(
        &connection.endpoint,
        &WorkerRequest::Interrupt {
            protocol_version: PROTOCOL_VERSION,
            run_key: connection.run_key.clone(),
            capability: read_capability(paths, run_id)?,
        },
    )? {
        WorkerResponse::InterruptAccepted { run_id: accepted } if accepted == run_id => Ok(()),
        WorkerResponse::Error { code, message } => Err(format!("{code}: {message}").into()),
        _ => Err("worker returned an unexpected interrupt response".into()),
    }
}

pub fn steer(
    paths: &SkeinPaths,
    run_id: i64,
    client_request_id: &str,
    prompt: String,
) -> Result<(i64, bool), Box<dyn std::error::Error>> {
    let mut registry = Registry::open(paths)?;
    recover_expired(&mut registry, paths)?;
    let connection = registry.worker_connection(run_id)?;
    match request(
        &connection.endpoint,
        &WorkerRequest::Steer {
            protocol_version: PROTOCOL_VERSION,
            run_key: connection.run_key.clone(),
            capability: read_capability(paths, run_id)?,
            client_request_id: client_request_id.to_owned(),
            prompt,
        },
    )? {
        WorkerResponse::SteerAccepted {
            run_id: accepted,
            action_id,
            queued,
        } if accepted == run_id => Ok((action_id, queued)),
        WorkerResponse::Error { code, message } => Err(format!("{code}: {message}").into()),
        _ => Err("worker returned an unexpected steer response".into()),
    }
}

pub fn read_source(paths: &SkeinPaths, run_id: i64) -> Result<Value, Box<dyn std::error::Error>> {
    let mut registry = Registry::open(paths)?;
    recover_expired(&mut registry, paths)?;
    let run = registry
        .control_run(run_id)?
        .ok_or_else(|| format!("worker run {run_id} was not found"))?;
    let thread_id = run
        .source_thread_id
        .as_deref()
        .ok_or("worker run has no authoritative Codex thread identity")?;
    let mut client = ControlClient::connect()?;
    let observed = client.read_thread(thread_id, &[])?;
    Ok(serde_json::json!({
        "threadId": observed.thread_id,
        "threadStatus": observed.status,
        "turns": observed.turns.into_iter().map(|turn| serde_json::json!({
            "turnId": turn.turn_id,
            "status": turn.status,
            "itemsFull": turn.items_full,
            "contentRedacted": true
        })).collect::<Vec<_>>(),
        "contentRedacted": true
    }))
}

pub fn reconcile(
    paths: &SkeinPaths,
    run_id: i64,
    client_request_id: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut registry = Registry::open(paths)?;
    recover_expired(&mut registry, paths)?;
    let plan = registry.plan_reconciliation(run_id, client_request_id)?;
    if !plan.should_dispatch {
        let run = registry
            .control_run(run_id)?
            .ok_or("worker run disappeared after reconciliation retry")?;
        return Ok(serde_json::json!({
            "requestId": client_request_id,
            "reused": true,
            "run": run
        }));
    }
    let detail = registry.control_run_detail(run_id)?;
    let mut known_ids = vec![plan.initial_client_message_id.clone()];
    known_ids.extend(
        detail
            .actions
            .iter()
            .filter(|action| action.action_kind == skein_core::ControlActionKind::TurnSteer)
            .filter_map(|action| action.client_request_id.clone()),
    );
    let known_refs = known_ids.iter().map(String::as_str).collect::<Vec<_>>();
    registry.begin_reconciliation(plan.action_id)?;
    let observed = match ControlClient::connect()
        .and_then(|mut client| client.read_thread(&plan.thread_id, &known_refs))
    {
        Ok(observed) => observed,
        Err(error) => {
            registry.fail_reconciliation(plan.action_id)?;
            return Err(error.into());
        }
    };
    let selected = match plan.expected_turn_id.as_deref() {
        Some(expected) => observed.turns.iter().find(|turn| turn.turn_id == expected),
        None => {
            let candidates = observed
                .turns
                .iter()
                .filter(|turn| {
                    turn.matched_client_message_ids
                        .contains(&plan.initial_client_message_id)
                })
                .collect::<Vec<_>>();
            if candidates.len() > 1 {
                registry.fail_reconciliation(plan.action_id)?;
                return Err("multiple Codex turns matched the initial client message id".into());
            }
            candidates.into_iter().next()
        }
    };
    let initial_observed = selected.is_some_and(|turn| {
        turn.matched_client_message_ids
            .contains(&plan.initial_client_message_id)
    });
    let observed_steers = selected
        .into_iter()
        .flat_map(|turn| turn.matched_client_message_ids.iter())
        .filter(|id| **id != plan.initial_client_message_id)
        .map(String::as_str)
        .collect::<Vec<_>>();
    let observation = ReconciliationObservation {
        thread_id: &observed.thread_id,
        thread_status: &observed.status,
        turn_id: selected.map(|turn| turn.turn_id.as_str()),
        turn_status: selected.map(|turn| turn.status.as_str()),
        initial_message_observed: initial_observed,
        observed_steer_client_ids: &observed_steers,
    };
    let run = registry.record_reconciliation(plan.action_id, &observation)?;
    Ok(serde_json::json!({
        "requestId": client_request_id,
        "reused": false,
        "run": run,
        "source": {
            "threadId": observed.thread_id,
            "threadStatus": observed.status,
            "turnId": observation.turn_id,
            "turnStatus": observation.turn_status,
            "contentRedacted": true
        }
    }))
}

pub fn durable_snapshot(
    paths: &SkeinPaths,
    run_id: i64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut registry = Registry::open(paths)?;
    recover_expired(&mut registry, paths)?;
    let run = registry
        .control_run(run_id)?
        .ok_or_else(|| format!("worker run {run_id} was not found"))?;
    Ok(serde_json::json!({
        "run": run,
        "worker": registry.control_worker(run_id)?,
        "liveEventsAvailable": registry.worker_connection(run_id).is_ok()
    }))
}

pub fn durable_list(
    paths: &SkeinPaths,
    active_only: bool,
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut registry = Registry::open(paths)?;
    recover_expired(&mut registry, paths)?;
    let mut jobs = Vec::new();
    for run in registry.list_control_runs()? {
        let Some(worker) = registry.control_worker(run.id)? else {
            continue;
        };
        if active_only
            && !matches!(
                worker.state,
                WorkerState::Starting
                    | WorkerState::Ready
                    | WorkerState::Busy
                    | WorkerState::Stopping
            )
        {
            continue;
        }
        jobs.push(serde_json::json!({"run": run, "worker": worker}));
    }
    Ok(Value::Array(jobs))
}

pub fn watch(
    paths: &SkeinPaths,
    run_id: i64,
    jsonl: bool,
) -> Result<ControlRun, Box<dyn std::error::Error>> {
    let mut sequence = 0;
    loop {
        match snapshot(paths, run_id, sequence) {
            Ok(snapshot) => {
                if snapshot.events_truncated {
                    if jsonl {
                        println!(
                            "{}",
                            serde_json::to_string(&serde_json::json!({
                                "type": "event_gap",
                                "requestedAfter": sequence,
                                "oldestAvailable": snapshot.oldest_sequence,
                                "contentPersisted": false
                            }))?
                        );
                    } else {
                        eprintln!(
                            "live event history was truncated; reconnect resumes at sequence {}",
                            snapshot.oldest_sequence.unwrap_or(snapshot.latest_sequence)
                        );
                    }
                }
                sequence = snapshot.latest_sequence;
                for event in snapshot.events {
                    if jsonl {
                        println!("{}", serde_json::to_string(&event)?);
                    } else {
                        print_event(&event);
                    }
                }
                if terminal(snapshot.run.state) {
                    return Ok(snapshot.run);
                }
            }
            Err(error) => {
                let mut registry = Registry::open(paths)?;
                recover_expired(&mut registry, paths)?;
                let run = registry
                    .control_run(run_id)?
                    .ok_or("worker run was not found")?;
                if terminal(run.state) || run.state == ControlRunState::RecoveryRequired {
                    return Ok(run);
                }
                return Err(error);
            }
        }
        thread::sleep(Duration::from_millis(200));
    }
}

pub fn serve(paths: SkeinPaths, run_id: i64) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = listener.local_addr()?.to_string();
    let capability = create_capability(&paths, run_id)?;
    let mut registry = Registry::open(&paths)?;
    let claim = registry.worker_claim(run_id)?;
    registry.mark_worker_ready(&claim, &endpoint, std::process::id())?;

    let shared = Arc::new(SharedState::default());
    let mut last_heartbeat = Instant::now();
    let mut terminal_since = None;
    let submission_deadline = Instant::now() + SUBMISSION_TIMEOUT;
    loop {
        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            let durable_worker = registry
                .control_worker(run_id)?
                .ok_or("worker row disappeared")?;
            if matches!(
                durable_worker.state,
                WorkerState::Exited | WorkerState::Lost
            ) {
                remove_capability(&paths, run_id);
                return Ok(());
            }
            let state = if shared.submitted.load(Ordering::SeqCst) {
                let run = registry
                    .control_run(run_id)?
                    .ok_or("worker run disappeared")?;
                if terminal(run.state) || run.state == ControlRunState::RecoveryRequired {
                    terminal_since.get_or_insert_with(Instant::now);
                    WorkerState::Ready
                } else {
                    WorkerState::Busy
                }
            } else {
                WorkerState::Ready
            };
            registry.heartbeat_worker(&claim, state)?;
            last_heartbeat = Instant::now();
        }

        match listener.accept() {
            Ok((mut stream, _)) => {
                let request_paths = paths.clone();
                let request_claim = claim.clone();
                let request_capability = capability.clone();
                let request_shared = Arc::clone(&shared);
                thread::spawn(move || {
                    let response = handle_request(
                        &request_paths,
                        &request_claim,
                        &request_capability,
                        &request_shared,
                        &mut stream,
                    )
                    .unwrap_or_else(|_| WorkerResponse::Error {
                        code: "request_failed".to_owned(),
                        message: "worker request failed safely".to_owned(),
                    });
                    let _ = write_response(&mut stream, &response);
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }

        if shared.shutdown.load(Ordering::SeqCst) {
            registry.finish_worker(&claim, "clean")?;
            remove_capability(&paths, run_id);
            return Ok(());
        }

        if !shared.submitted.load(Ordering::SeqCst) && Instant::now() >= submission_deadline {
            registry.fail_worker_without_submission(&claim)?;
            remove_capability(&paths, run_id);
            return Ok(());
        }

        if terminal_since.is_some_and(|since| since.elapsed() >= TERMINAL_IDLE_TIMEOUT) {
            registry.finish_worker(&claim, "clean")?;
            remove_capability(&paths, run_id);
            return Ok(());
        }
        thread::sleep(POLL_INTERVAL);
    }
}

pub fn codex_guard() -> Result<(), Box<dyn std::error::Error>> {
    let codex = std::env::var_os("SKEIN_GUARDED_CODEX_BIN").unwrap_or_else(|| "codex".into());
    let mut child = Command::new(codex)
        .args(["app-server", "--listen", "stdio://"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut child_stdin = child
        .stdin
        .take()
        .ok_or("guarded Codex stdin was unavailable")?;
    let mut child_stdout = child
        .stdout
        .take()
        .ok_or("guarded Codex stdout was unavailable")?;
    let output = thread::spawn(move || -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        io::copy(&mut child_stdout, &mut stdout)?;
        stdout.flush()
    });
    let _ = io::copy(&mut io::stdin().lock(), &mut child_stdin);
    drop(child_stdin);
    let _ = child.kill();
    let _ = child.wait();
    let _ = output.join();
    Ok(())
}

fn handle_request(
    paths: &SkeinPaths,
    claim: &WorkerClaim,
    expected_capability: &str,
    shared: &Arc<SharedState>,
    stream: &mut TcpStream,
) -> Result<WorkerResponse, Box<dyn std::error::Error>> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut body = String::new();
    stream
        .take((MAX_IPC_REQUEST_BYTES + 1) as u64)
        .read_to_string(&mut body)?;
    if body.len() > MAX_IPC_REQUEST_BYTES {
        return Ok(WorkerResponse::Error {
            code: "request_too_large".to_owned(),
            message: "worker request exceeded the protocol limit".to_owned(),
        });
    }
    let request: WorkerRequest = serde_json::from_str(&body)?;
    let (version, run_key, capability) = match &request {
        WorkerRequest::Submit {
            protocol_version,
            run_key,
            capability,
            ..
        }
        | WorkerRequest::Snapshot {
            protocol_version,
            run_key,
            capability,
            ..
        }
        | WorkerRequest::Shutdown {
            protocol_version,
            run_key,
            capability,
        }
        | WorkerRequest::Interrupt {
            protocol_version,
            run_key,
            capability,
        }
        | WorkerRequest::Steer {
            protocol_version,
            run_key,
            capability,
            ..
        } => (*protocol_version, run_key, capability),
    };
    if version != PROTOCOL_VERSION
        || run_key != claim.run_key()
        || capability != expected_capability
    {
        return Ok(WorkerResponse::Error {
            code: "authentication_failed".to_owned(),
            message: "worker handshake was rejected".to_owned(),
        });
    }

    match request {
        WorkerRequest::Submit { prompt, .. } => {
            if shared
                .submitted
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return Ok(WorkerResponse::Error {
                    code: "already_submitted".to_owned(),
                    message: "this worker already owns a submitted turn".to_owned(),
                });
            }
            let paths = paths.clone();
            let claim = claim.clone();
            let run_id = claim.run_id();
            let shared = Arc::clone(shared);
            thread::spawn(move || run_codex_job(paths, claim, prompt, shared));
            Ok(WorkerResponse::Accepted { run_id })
        }
        WorkerRequest::Snapshot { after_sequence, .. } => {
            let registry = Registry::open(paths)?;
            let run = registry
                .control_run(claim.run_id())?
                .ok_or("worker run disappeared")?;
            let worker = registry
                .control_worker(claim.run_id())?
                .ok_or("worker row disappeared")?;
            let (events, latest_sequence, oldest_sequence, events_truncated) =
                shared.snapshot(after_sequence);
            Ok(WorkerResponse::Snapshot(Box::new(WorkerSnapshot {
                run,
                worker,
                events,
                latest_sequence,
                oldest_sequence,
                events_truncated,
            })))
        }
        WorkerRequest::Shutdown { .. } => {
            let registry = Registry::open(paths)?;
            let run = registry
                .control_run(claim.run_id())?
                .ok_or("worker run disappeared")?;
            if !terminal(run.state) && run.state != ControlRunState::RecoveryRequired {
                return Ok(WorkerResponse::Error {
                    code: "active_run".to_owned(),
                    message: "worker refuses shutdown while its run is active".to_owned(),
                });
            }
            shared.shutdown.store(true, Ordering::SeqCst);
            Ok(WorkerResponse::Stopped {
                run_id: claim.run_id(),
            })
        }
        WorkerRequest::Interrupt { .. } => {
            let mut commands = shared
                .commands
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let mut registry = Registry::open(paths)?;
            let interrupt = registry.plan_owned_interrupt(claim.run_id(), claim)?;
            if interrupt.should_dispatch
                && shared
                    .interrupt_queued
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                commands.push_back(RuntimeCommand::Interrupt(interrupt));
            }
            Ok(WorkerResponse::InterruptAccepted {
                run_id: claim.run_id(),
            })
        }
        WorkerRequest::Steer {
            client_request_id,
            prompt,
            ..
        } => {
            let mut commands = shared
                .commands
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let mut registry = Registry::open(paths)?;
            let plan = registry.plan_owned_steer(
                claim.run_id(),
                &client_request_id,
                prompt.len(),
                claim,
            )?;
            let action_id = plan.action_id;
            let queued = plan.should_dispatch;
            if queued {
                commands.push_back(RuntimeCommand::Steer { plan, prompt });
            }
            Ok(WorkerResponse::SteerAccepted {
                run_id: claim.run_id(),
                action_id,
                queued,
            })
        }
    }
}

fn run_codex_job(paths: SkeinPaths, claim: WorkerClaim, prompt: String, shared: Arc<SharedState>) {
    if run_codex_job_inner(&paths, &claim, &prompt, &shared).is_err() {
        shared
            .commands
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        if let Ok(mut registry) = Registry::open(&paths)
            && registry.fail_worker_without_submission(&claim).is_err()
        {
            let _ = registry.mark_owned_control_uncertain(claim.run_id(), &claim);
        }
    }
}

fn run_codex_job_inner(
    paths: &SkeinPaths,
    claim: &WorkerClaim,
    prompt: &str,
    shared: &SharedState,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut registry = Registry::open(paths)?;
    registry.heartbeat_worker(claim, WorkerState::Busy)?;
    let plan = registry.control_plan(claim.run_id())?;
    let run = registry
        .control_run(claim.run_id())?
        .ok_or("worker run disappeared")?;
    let executable = std::env::current_exe()?;
    let mut client = ControlClient::connect_guarded(&executable)?;

    registry.begin_owned_control_action(plan.thread_action_id, claim)?;
    let thread = match run.source_thread_id.as_deref() {
        Some(thread_id) => client.resume_thread(thread_id, &plan.working_directory),
        None => client.start_thread(&plan.working_directory),
    }?;
    registry.acknowledge_owned_thread_action(
        plan.thread_action_id,
        &thread.thread_id,
        Some(&thread.session_id),
        claim,
    )?;
    registry.begin_owned_control_action(plan.turn_action_id, claim)?;
    let turn = client.start_turn(
        &thread.thread_id,
        prompt,
        &plan.client_message_id,
        &plan.working_directory,
    )?;
    registry.acknowledge_owned_turn_action(plan.turn_action_id, &turn.turn_id, claim)?;

    loop {
        if let Some(command) = shared
            .commands
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
        {
            match command {
                RuntimeCommand::Interrupt(interrupt) => {
                    registry.begin_owned_control_action(interrupt.action_id, claim)?;
                    client.interrupt_turn(&interrupt.thread_id, &interrupt.turn_id)?;
                    registry.acknowledge_owned_interrupt(
                        interrupt.action_id,
                        &interrupt.turn_id,
                        claim,
                    )?;
                }
                RuntimeCommand::Steer { plan, prompt } => {
                    registry.begin_owned_control_action(plan.action_id, claim)?;
                    match client.steer_turn(
                        &plan.thread_id,
                        &plan.turn_id,
                        &prompt,
                        &plan.client_request_id,
                    ) {
                        Ok(()) => registry.acknowledge_owned_steer(
                            plan.action_id,
                            &plan.turn_id,
                            claim,
                        )?,
                        Err(skein_codex::Error::Server { .. }) => {
                            registry.reject_owned_steer(plan.action_id, claim)?;
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
            }
        }
        let Some(event) = client.next_event_timeout(Duration::from_millis(200))? else {
            continue;
        };
        if event_matches(&event, &thread.thread_id, &turn.turn_id) {
            shared.push(&event);
        }
        if let ControlEvent::TurnCompleted {
            thread_id,
            turn_id,
            status,
        } = event
            && thread_id == thread.thread_id
            && turn_id == turn.turn_id
        {
            registry.complete_owned_control_run(claim.run_id(), &status, claim)?;
            shared
                .commands
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clear();
            registry.heartbeat_worker(claim, WorkerState::Ready)?;
            return Ok(());
        }
    }
}

fn request(
    endpoint: &str,
    request: &WorkerRequest,
) -> Result<WorkerResponse, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(endpoint)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.shutdown(Shutdown::Write)?;
    let mut body = String::new();
    stream
        .take((MAX_IPC_REQUEST_BYTES + 1) as u64)
        .read_to_string(&mut body)?;
    if body.len() > MAX_IPC_REQUEST_BYTES {
        return Err("worker response exceeded the protocol limit".into());
    }
    Ok(serde_json::from_str(&body)?)
}

fn write_response(
    stream: &mut TcpStream,
    response: &WorkerResponse,
) -> Result<(), Box<dyn std::error::Error>> {
    serde_json::to_writer(&mut *stream, response)?;
    stream.flush()?;
    Ok(())
}

fn await_connection(
    paths: &SkeinPaths,
    run_id: i64,
) -> Result<skein_core::WorkerConnection, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let registry = Registry::open(paths)?;
        if let Ok(connection) = registry.worker_connection(run_id) {
            return Ok(connection);
        }
        if Instant::now() >= deadline {
            return Err("worker did not publish a ready endpoint in time".into());
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn capability_path(paths: &SkeinPaths, run_id: i64) -> std::path::PathBuf {
    paths
        .data_dir
        .join("workers")
        .join(format!("run-{run_id}.capability"))
}

#[cfg(unix)]
fn configure_detached_worker(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn configure_detached_worker(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
fn configure_detached_worker(_command: &mut Command) {}

pub(crate) fn recover_expired(
    registry: &mut Registry,
    paths: &SkeinPaths,
) -> Result<(), Box<dyn std::error::Error>> {
    for run_id in registry.recover_expired_workers()? {
        remove_capability(paths, run_id);
    }
    Ok(())
}

fn create_capability(
    paths: &SkeinPaths,
    run_id: i64,
) -> Result<String, Box<dyn std::error::Error>> {
    let directory = paths.data_dir.join("workers");
    fs::create_dir_all(&directory)?;
    set_private_directory(&directory)?;
    let path = capability_path(paths, run_id);
    let capability = Uuid::new_v4().to_string();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    set_private_file(&path)?;
    file.write_all(capability.as_bytes())?;
    file.sync_all()?;
    Ok(capability)
}

fn read_capability(paths: &SkeinPaths, run_id: i64) -> Result<String, Box<dyn std::error::Error>> {
    let value = fs::read_to_string(capability_path(paths, run_id))?;
    let value = value.trim();
    if value.len() != 36
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        return Err("worker capability file was malformed".into());
    }
    Ok(value.to_owned())
}

fn remove_capability(paths: &SkeinPaths, run_id: i64) {
    let _ = fs::remove_file(capability_path(paths, run_id));
}

#[cfg(unix)]
fn set_private_directory(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_directory(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_file(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

fn redact_event(sequence: u64, event: &ControlEvent) -> RedactedWorkerEvent {
    match event {
        ControlEvent::TurnStarted { thread_id, turn_id } => RedactedWorkerEvent {
            sequence,
            kind: "turn_started".to_owned(),
            thread_id: Some(thread_id.clone()),
            turn_id: Some(turn_id.clone()),
            status: None,
            item_type: None,
            delta_bytes: None,
        },
        ControlEvent::AgentMessageDelta {
            thread_id,
            turn_id,
            delta,
        } => RedactedWorkerEvent {
            sequence,
            kind: "agent_message_delta".to_owned(),
            thread_id: Some(thread_id.clone()),
            turn_id: Some(turn_id.clone()),
            status: None,
            item_type: None,
            delta_bytes: Some(delta.len()),
        },
        ControlEvent::ItemStarted {
            thread_id,
            turn_id,
            item_type,
        }
        | ControlEvent::ItemCompleted {
            thread_id,
            turn_id,
            item_type,
        } => RedactedWorkerEvent {
            sequence,
            kind: if matches!(event, ControlEvent::ItemStarted { .. }) {
                "item_started"
            } else {
                "item_completed"
            }
            .to_owned(),
            thread_id: Some(thread_id.clone()),
            turn_id: Some(turn_id.clone()),
            status: None,
            item_type: Some(item_type.clone()),
            delta_bytes: None,
        },
        ControlEvent::ThreadStatusChanged { thread_id, status } => RedactedWorkerEvent {
            sequence,
            kind: "thread_status_changed".to_owned(),
            thread_id: Some(thread_id.clone()),
            turn_id: None,
            status: Some(status.clone()),
            item_type: None,
            delta_bytes: None,
        },
        ControlEvent::RetryingError {
            thread_id,
            turn_id,
            will_retry,
        } => RedactedWorkerEvent {
            sequence,
            kind: "retrying_error".to_owned(),
            thread_id: Some(thread_id.clone()),
            turn_id: Some(turn_id.clone()),
            status: Some(
                if *will_retry {
                    "retrying"
                } else {
                    "not_retrying"
                }
                .to_owned(),
            ),
            item_type: None,
            delta_bytes: None,
        },
        ControlEvent::TurnCompleted {
            thread_id,
            turn_id,
            status,
        } => RedactedWorkerEvent {
            sequence,
            kind: "turn_completed".to_owned(),
            thread_id: Some(thread_id.clone()),
            turn_id: Some(turn_id.clone()),
            status: Some(status.clone()),
            item_type: None,
            delta_bytes: None,
        },
        ControlEvent::Unknown { method } => RedactedWorkerEvent {
            sequence,
            kind: format!("unknown:{method}"),
            thread_id: None,
            turn_id: None,
            status: None,
            item_type: None,
            delta_bytes: None,
        },
    }
}

fn event_matches(event: &ControlEvent, thread_id: &str, turn_id: &str) -> bool {
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

fn terminal(state: ControlRunState) -> bool {
    matches!(
        state,
        ControlRunState::Completed | ControlRunState::Failed | ControlRunState::Interrupted
    )
}

fn print_event(event: &RedactedWorkerEvent) {
    match event.kind.as_str() {
        "agent_message_delta" => print!("·"),
        "turn_started" => eprintln!("Codex turn started"),
        "turn_completed" => eprintln!(
            "\nCodex turn {}",
            event.status.as_deref().unwrap_or("ended")
        ),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_event_window_reports_a_reconnect_gap() {
        let shared = SharedState::default();
        for index in 0..=MAX_EVENTS {
            shared.push(&ControlEvent::Unknown {
                method: format!("synthetic/{index}"),
            });
        }
        let (events, latest, oldest, truncated) = shared.snapshot(0);
        assert_eq!(events.len(), MAX_EVENTS);
        assert_eq!(latest, (MAX_EVENTS + 1) as u64);
        assert_eq!(oldest, Some(2));
        assert!(truncated);
    }
}
