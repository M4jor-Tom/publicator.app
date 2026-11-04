use crate::{
    application::Application,
    error::{Error, Result},
    images::prepare_upload,
    model::{Phase, phase},
    publish_journal::{self, PublishJournal},
    publish_protocol::{PublishJob, ScheduleParts, SessionOpen, WorkerCommand, WorkerEvent},
    scheduling,
    store::Store,
};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, Command},
    time::Instant,
};

const MAX_LINE: usize = 1024 * 1024;
// ponytail: fixed per-worker timeouts and a 1 MiB frame cap; tune only when real browser behavior requires it.
#[derive(Clone, Copy)]
struct Timeouts {
    check_steps: Duration,
    ready: Duration,
    artwork: Duration,
    cleanup: Duration,
}
impl Default for Timeouts {
    fn default() -> Self {
        Self {
            check_steps: Duration::from_secs(60),
            ready: Duration::from_secs(90),
            artwork: Duration::from_secs(600),
            cleanup: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PublishCell {
    pub uuid: String,
    pub platform: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishCellState {
    Success,
    SafeFailed,
    Unknown,
    Unattempted,
    ConfirmedNotSubmitted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishCellOutcome {
    pub cell: PublishCell,
    pub state: PublishCellState,
    pub detail: Option<String>,
}

impl PublishCellOutcome {
    pub fn retryable(&self) -> bool {
        matches!(
            &self.state,
            PublishCellState::SafeFailed
                | PublishCellState::Unattempted
                | PublishCellState::ConfirmedNotSubmitted
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishCompletion {
    pub result: std::result::Result<PublishReport, String>,
    pub diagnostics: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PublishRequest {
    pub uuids: Option<Vec<String>>,
    pub headless: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PublishReport {
    pub published: usize,
    pub failed: usize,
    pub unknown: usize,
    pub unattempted: usize,
    pub error: Option<String>,
    pub outcomes: Vec<PublishCellOutcome>,
}
pub struct Publisher {
    worker: PathBuf,
    skill: PathBuf,
    timeouts: Timeouts,
}
impl Publisher {
    pub fn new(worker: PathBuf, skill: PathBuf) -> Self {
        Self {
            worker,
            skill,
            timeouts: Timeouts::default(),
        }
    }
    /// `PUBLICATOR_WORKER`/`PUBLICATOR_SKILL`, else the checkout-relative development assets.
    pub fn from_env() -> Self {
        let path = |name, default| {
            std::env::var_os(name).map_or_else(|| PathBuf::from(default), PathBuf::from)
        };
        Self::new(
            path("PUBLICATOR_WORKER", "python/publicator-da-worker"),
            path(
                "PUBLICATOR_SKILL",
                ".claude/skills/publish-deviantart/SKILL.md",
            ),
        )
    }
    #[cfg(test)]
    fn with_timeouts(mut self, timeouts: Timeouts) -> Self {
        self.timeouts = timeouts;
        self
    }
    pub async fn check_steps(&self) -> Result<()> {
        let mut command = Command::new(&self.worker);
        command.arg("--check-steps").arg("--skill").arg(&self.skill);
        let status = OwnedProcess::spawn(&mut command)?
            .shutdown(self.timeouts.check_steps, self.timeouts.cleanup)
            .await
            .ok_or_else(|| Error::Protocol("publishing step check timed out".into()))??;
        if status.success() {
            Ok(())
        } else {
            Err(Error::Validation(
                "publishing steps differ from skill".into(),
            ))
        }
    }
    pub async fn publish(
        &self,
        app: &Application,
        request: PublishRequest,
        cancel: Arc<AtomicBool>,
    ) -> Result<PublishReport> {
        self.publish_inner(app, request, cancel, None).await
    }

    async fn publish_inner(
        &self,
        app: &Application,
        request: PublishRequest,
        cancel: Arc<AtomicBool>,
        expected_cells: Option<&[PublishCell]>,
    ) -> Result<PublishReport> {
        let execution = self
            .publish_inner_with_diagnostics(app, request, cancel, expected_cells)
            .await;
        if let (Err(_), Some(detail)) = (&execution.result, &execution.diagnostics) {
            eprintln!("publisher worker stderr (original error preserved): {detail}");
        }
        execution.result
    }

    async fn publish_inner_with_diagnostics(
        &self,
        app: &Application,
        request: PublishRequest,
        cancel: Arc<AtomicBool>,
        expected_cells: Option<&[PublishCell]>,
    ) -> PublishExecution {
        let mut worker_diagnostics = None;
        let result = async {
            let _lease = app.begin_publishing()?;
            let store = app.store();
            let (journal, _lock) = locked_journal(store)?;
            let rows = store.read()?;
            let targets = selected_targets(&rows, request.uuids.as_deref(), expected_cells)?;
            if targets.is_empty() {
                return Ok(PublishReport::default());
            }
            let zone = scheduling::timezone(store.config())?;
            let batch = prepare_batch(store, targets, zone)?;
            self.check_steps().await?;
            let execution = self
                .execute_worker(
                    &journal,
                    store,
                    &batch.jobs,
                    request.headless,
                    zone,
                    &cancel,
                )
                .await?;
            worker_diagnostics = execution.diagnostics;
            execution.result
        }
        .await;
        PublishExecution {
            result,
            diagnostics: worker_diagnostics,
        }
    }
    async fn execute_worker(
        &self,
        journal: &PublishJournal,
        store: &Store,
        batch: &[(publish_journal::ApprovedSnapshot, PublishJob)],
        headless: bool,
        zone: chrono_tz::Tz,
        cancel: &AtomicBool,
    ) -> Result<PublishExecution> {
        let mut worker = start_worker(&self.worker, &store.paths().data_dir)?;
        let mut drain = tokio::task::JoinSet::new();
        let diagnostics = drain_stderr(&mut worker.child, &mut drain);
        let result = self
            .run_batch(
                &mut worker.child,
                journal,
                store,
                batch,
                headless,
                zone,
                self.timeouts,
                cancel,
            )
            .await;
        // `run_batch` sent `close` or dropped stdin: either way `serve` closes the browser
        // session on its way out, so the worker gets a grace period before any signal.
        worker
            .shutdown(self.timeouts.cleanup, self.timeouts.cleanup)
            .await;
        // A descendant that left the worker group may hold stderr open: keep what was read
        // by the deadline; dropping the set aborts the drain.
        let _ = tokio::time::timeout(self.timeouts.cleanup, drain.join_next()).await;
        drop(drain);
        let detail = String::from_utf8_lossy(
            &diagnostics
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
        .into_owned();
        let (result, diagnostics) = with_worker_diagnostics(result, &detail);
        Ok(PublishExecution {
            result,
            diagnostics,
        })
    }

    // This function coordinates the batch, process, Store, journal, and bounded worker lifecycle.
    #[allow(clippy::too_many_arguments)]
    async fn run_batch(
        &self,
        child: &mut Child,
        journal: &PublishJournal,
        store: &Store,
        batch: &[(publish_journal::ApprovedSnapshot, PublishJob)],
        headless: bool,
        zone: chrono_tz::Tz,
        timeouts: Timeouts,
        cancel: &AtomicBool,
    ) -> Result<PublishReport> {
        let (mut input, mut output) =
            open_worker(child, store, headless, zone, timeouts.ready).await?;
        let mut report = PublishReport::default();
        for (index, (approved, job)) in batch.iter().enumerate() {
            let cell = PublishCell {
                uuid: job.uuid.clone(),
                platform: "deviantart".into(),
            };
            if !approved_for_dispatch(store, approved, job, cancel)? {
                record_unattempted(&mut report, batch, index);
                break;
            }
            journal.begin(job, approved)?;
            let event = match submit_job(&mut input, &mut output, job, timeouts.artwork).await {
                Ok(event) => event,
                Err(detail) => {
                    report.error = Some(detail.clone());
                    record_outcome(&mut report, cell, PublishCellState::Unknown, Some(detail));
                    record_unattempted(&mut report, batch, index + 1);
                    break;
                }
            };
            if !record_terminal(journal, store, &mut report, cell, event)? {
                record_unattempted(&mut report, batch, index + 1);
                break;
            }
        }
        let _ = tokio::time::timeout(
            timeouts.cleanup,
            send_line(&mut input, &WorkerCommand::Close { protocol: 1 }),
        )
        .await;
        Ok(report)
    }
}
fn approved_for_dispatch(
    store: &Store,
    approved: &publish_journal::ApprovedSnapshot,
    job: &PublishJob,
    cancel: &AtomicBool,
) -> Result<bool> {
    if cancel.load(Ordering::SeqCst) {
        return Ok(false);
    }
    // A delivery recorded by another writer since approval is no longer sendable.
    Ok(publish_journal::pending_approval(store, &job.uuid)?
        .is_some_and(|(current, _)| current == *approved))
}

fn locked_journal(store: &Store) -> Result<(PublishJournal, publish_journal::PublisherLock)> {
    let journal = PublishJournal::new(&store.paths().data_dir)?;
    let lock = publish_journal::publisher_lock(&store.paths().data_dir)?;
    let recovery = journal.recover(store)?;
    if !recovery.blocked.is_empty() {
        return Err(Error::ReconciliationRequired(
            "unresolved publishing attempts require reconciliation".into(),
        ));
    }
    Ok((journal, lock))
}

fn record_terminal(
    journal: &PublishJournal,
    store: &Store,
    report: &mut PublishReport,
    cell: PublishCell,
    event: WorkerEvent,
) -> Result<bool> {
    journal.terminal(&event)?;
    match event {
        WorkerEvent::Scheduled { attempt_id, .. } => {
            journal.apply_scheduled(store, &attempt_id)?;
            record_outcome(report, cell, PublishCellState::Success, None);
        }
        WorkerEvent::Failed { error, .. } => {
            report.error = Some(error.clone());
            record_outcome(report, cell, PublishCellState::SafeFailed, Some(error));
            return Ok(false);
        }
        WorkerEvent::Unknown { error, .. } => {
            report.error = Some(error.clone());
            record_outcome(report, cell, PublishCellState::Unknown, Some(error));
            return Ok(false);
        }
        _ => return Err(Error::Protocol("unexpected worker terminal".into())),
    }
    Ok(true)
}

async fn submit_job(
    input: &mut (impl AsyncWrite + Unpin),
    output: &mut (impl AsyncBufRead + Unpin),
    job: &PublishJob,
    artwork_timeout: Duration,
) -> std::result::Result<WorkerEvent, String> {
    let deadline = Instant::now() + artwork_timeout;
    tokio::time::timeout_at(
        deadline,
        send_line(input, &WorkerCommand::Submit(job.clone())),
    )
    .await
    .map_err(|error| {
        Error::Protocol(format!(
            "worker submit write timeout; result is unknown: {error}"
        ))
        .to_string()
    })?
    .map_err(|error| error.to_string())?;
    match tokio::time::timeout_at(deadline, read_matching_terminal(output, job, deadline)).await {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(_) => Err("worker artwork timeout; result is unknown".into()),
    }
}

async fn open_worker(
    child: &mut Child,
    store: &Store,
    headless: bool,
    zone: chrono_tz::Tz,
    ready_timeout: Duration,
) -> Result<(
    tokio::process::ChildStdin,
    BufReader<tokio::process::ChildStdout>,
)> {
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| Error::Protocol("worker stdin unavailable".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Protocol("worker stdout unavailable".into()))?;
    let mut output = BufReader::new(stdout);
    let data = store.paths().data_dir.clone();
    let open = WorkerCommand::Open(SessionOpen {
        protocol: 1,
        login_profile: data.join(".deviantart-login"),
        work_profile: data.join(".deviantart-session"),
        headless,
        timezone: zone.to_string(),
    });
    let ready_deadline = Instant::now() + ready_timeout;
    tokio::time::timeout_at(ready_deadline, send_line(&mut input, &open))
        .await
        .map_err(|_| Error::Protocol("worker open timeout".into()))??;
    match tokio::time::timeout_at(ready_deadline, read_event(&mut output))
        .await
        .map_err(|_| Error::Protocol("worker ready timeout".into()))??
    {
        WorkerEvent::Ready { protocol: 1 } => {}
        WorkerEvent::Fatal { error, .. } => return Err(Error::Protocol(error)),
        _ => return Err(Error::Protocol("worker did not become ready".into())),
    }
    Ok((input, output))
}

/// Keeps the first 8 KiB of worker stderr while draining the rest so the worker never blocks.
fn drain_stderr(
    child: &mut Child,
    tasks: &mut tokio::task::JoinSet<()>,
) -> Arc<std::sync::Mutex<Vec<u8>>> {
    let retained = Arc::<std::sync::Mutex<Vec<u8>>>::default();
    if let Some(mut stderr) = child.stderr.take() {
        let retained = Arc::clone(&retained);
        tasks.spawn(async move {
            let mut chunk = [0u8; 4096];
            while let Ok(count @ 1..) = stderr.read(&mut chunk).await {
                let mut buffer = retained
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let room = 8192usize.saturating_sub(buffer.len());
                buffer.extend_from_slice(&chunk[..count.min(room)]);
            }
        });
    }
    retained
}

struct PreparedBatch {
    jobs: Vec<(publish_journal::ApprovedSnapshot, PublishJob)>,
    // Keep staged uploads alive through worker shutdown and diagnostic draining.
    _staged_uploads: Vec<TempDir>,
}

fn prepare_batch(
    store: &Store,
    targets: Vec<&Value>,
    zone: chrono_tz::Tz,
) -> Result<PreparedBatch> {
    let mut batch = Vec::new();
    let mut staged_uploads: Vec<TempDir> = Vec::new();
    for row in targets {
        let uuid = row
            .get("uuid")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Validation("publication UUID missing".into()))?;
        // Snapshot and upload derive from one fresh resolution.
        let (approved, artwork) = publish_journal::pending_approval(store, uuid)?
            .ok_or_else(|| Error::InvalidState("delivery target is no longer pending".into()))?;
        // The worker opens only this verified per-attempt copy, never the mutable original.
        let staging = tempfile::tempdir_in(store.paths().data_dir.join(".publicator"))?;
        let upload = prepare_upload(&artwork.stage(staging.path())?, staging.path())?;
        let attempt_id = uuid::Uuid::new_v4().to_string();
        let job = make_job(&approved, attempt_id, upload, zone)?;
        staged_uploads.push(staging);
        job_command_valid(&job)?;
        batch.push((approved, job));
    }
    Ok(PreparedBatch {
        jobs: batch,
        _staged_uploads: staged_uploads,
    })
}

fn selected_targets<'a>(
    rows: &'a [Value],
    uuids: Option<&[String]>,
    expected_cells: Option<&[PublishCell]>,
) -> Result<Vec<&'a Value>> {
    let targets = pending_targets(rows, uuids)?;
    if let Some(expected) = expected_cells {
        let actual = targets
            .iter()
            .filter_map(|row| row.get("uuid").and_then(Value::as_str))
            .map(|uuid| PublishCell {
                uuid: uuid.to_owned(),
                platform: "deviantart".into(),
            })
            .collect::<std::collections::HashSet<_>>();
        let expected_set = expected
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        if actual != expected_set || expected_set.len() != expected.len() {
            return Err(Error::InvalidState(
                "selected publishing cells changed before publisher admission".into(),
            ));
        }
    }
    Ok(targets)
}

struct PublishExecution {
    result: Result<PublishReport>,
    diagnostics: Option<String>,
}

fn record_outcome(
    report: &mut PublishReport,
    cell: PublishCell,
    state: PublishCellState,
    detail: Option<String>,
) {
    match state {
        PublishCellState::Success => report.published += 1,
        PublishCellState::SafeFailed => report.failed += 1,
        PublishCellState::Unknown => report.unknown += 1,
        PublishCellState::Unattempted | PublishCellState::ConfirmedNotSubmitted => {
            report.unattempted += 1
        }
    }
    report.outcomes.push(PublishCellOutcome {
        cell,
        state,
        detail,
    });
}

fn record_unattempted(
    report: &mut PublishReport,
    batch: &[(publish_journal::ApprovedSnapshot, PublishJob)],
    start: usize,
) {
    for (_, job) in batch.iter().skip(start) {
        record_outcome(
            report,
            PublishCell {
                uuid: job.uuid.clone(),
                platform: "deviantart".into(),
            },
            PublishCellState::Unattempted,
            None,
        );
    }
}

fn with_worker_diagnostics(
    result: Result<PublishReport>,
    detail: &str,
) -> (Result<PublishReport>, Option<String>) {
    let detail = detail.trim();
    if detail.is_empty() {
        return (result, None);
    }
    let result = match result {
        Ok(mut report) => {
            if let Some(error) = &mut report.error {
                error.push_str(&format!("; worker stderr: {detail}"));
            }
            Ok(report)
        }
        Err(error) => Err(error),
    };
    (result, Some(detail.to_owned()))
}
impl Application {
    pub fn with_publisher(mut self, publisher: Publisher) -> Self {
        self.publisher = Some(publisher);
        self
    }
    pub async fn publish(
        &self,
        request: PublishRequest,
        cancel: Arc<AtomicBool>,
    ) -> Result<PublishReport> {
        let publisher = self
            .publisher
            .as_ref()
            .ok_or_else(|| Error::InvalidState("publisher is not configured".into()))?;
        publisher.publish(self, request, cancel).await
    }

    pub async fn publish_selected(
        &self,
        cells: Vec<PublishCell>,
        cancel: Arc<AtomicBool>,
    ) -> Result<PublishReport> {
        let publisher = self
            .publisher
            .as_ref()
            .ok_or_else(|| Error::InvalidState("publisher is not configured".into()))?;
        let uuids = cells
            .iter()
            .map(|cell| cell.uuid.clone())
            .collect::<Vec<_>>();
        publisher
            .publish_inner(
                self,
                PublishRequest {
                    uuids: Some(uuids),
                    headless: false,
                },
                cancel,
                Some(&cells),
            )
            .await
    }

    pub async fn publish_selected_with_diagnostics(
        &self,
        cells: Vec<PublishCell>,
        cancel: Arc<AtomicBool>,
    ) -> PublishCompletion {
        let Some(publisher) = self.publisher.as_ref() else {
            return PublishCompletion {
                result: Err("invalid state: publisher is not configured".into()),
                diagnostics: None,
            };
        };
        let uuids = cells.iter().map(|cell| cell.uuid.clone()).collect();
        let execution = publisher
            .publish_inner_with_diagnostics(
                self,
                PublishRequest {
                    uuids: Some(uuids),
                    headless: false,
                },
                cancel,
                Some(&cells),
            )
            .await;
        PublishCompletion {
            result: execution.result.map_err(|error| error.to_string()),
            diagnostics: execution.diagnostics,
        }
    }
}
pub fn first_pending_uuid(rows: &[Value]) -> Result<Option<String>> {
    Ok(pending_targets(rows, None)?
        .first()
        .and_then(|row| row.get("uuid"))
        .and_then(Value::as_str)
        .map(str::to_owned))
}

fn pending_targets<'a>(rows: &'a [Value], uuids: Option<&[String]>) -> Result<Vec<&'a Value>> {
    let mut selected = Vec::new();
    for row in rows {
        let uuid = row
            .get("uuid")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Validation("publication UUID missing".into()))?;
        if uuids.is_some_and(|ids| !ids.iter().any(|id| id == uuid)) {
            continue;
        }
        let executable = matches!(phase(row)?, Phase::Described | Phase::Appeared)
            && row
                .get("apparitions")
                .and_then(Value::as_array)
                .is_some_and(|apps| {
                    apps.iter().any(|a| {
                        a.get("platformName").and_then(Value::as_str) == Some("deviantart")
                            && a.get("state").and_then(Value::as_str) == Some("unpublished")
                    })
                });
        if executable {
            selected.push(row);
        }
    }
    if let Some(ids) = uuids {
        for id in ids {
            if !rows
                .iter()
                .any(|r| r.get("uuid").and_then(Value::as_str) == Some(id))
            {
                return Err(Error::NotFound(id.clone()));
            }
        }
    }
    Ok(selected)
}
fn make_job(
    a: &publish_journal::ApprovedSnapshot,
    attempt_id: String,
    image_path: PathBuf,
    zone: chrono_tz::Tz,
) -> Result<PublishJob> {
    let dt = chrono::DateTime::from_timestamp(a.schedule_epoch, 0)
        .ok_or_else(|| Error::Validation("invalid schedule epoch".into()))?
        .with_timezone(&zone);
    let job = PublishJob {
        protocol: 1,
        attempt_id,
        uuid: a.uuid.clone(),
        image_path,
        title: a.title.clone(),
        description: a.description.clone(),
        tags: a.tags.clone(),
        schedule: ScheduleParts {
            epoch: a.schedule_epoch,
            timezone: zone.to_string(),
            year: dt.year(),
            month: dt.month(),
            day: dt.day(),
            hour: dt.hour(),
            minute: dt.minute(),
        },
        price: a.price,
        tiers: a.tiers.clone(),
        galleries: a.galleries.clone(),
    };
    Ok(job)
}
fn job_command_valid(job: &PublishJob) -> Result<()> {
    let command = WorkerCommand::Submit(job.clone());
    command.validate().map_err(Error::Validation)?;
    encode_line(&command).map(|_| ())
}
fn start_worker(path: &Path, data_root: &Path) -> Result<OwnedProcess> {
    let mut c = Command::new(path);
    c.arg("--data-root")
        .arg(data_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    OwnedProcess::spawn(&mut c)
}

/// A child leading its own process group (browser descendants included). `shutdown` is the
/// cooperative path; dropping it unawaited (an aborted or interrupted publish) SIGKILLs the group,
/// even mid-shutdown.
struct OwnedProcess {
    child: Child,
    group: Option<u32>,
}

impl OwnedProcess {
    fn spawn(command: &mut Command) -> Result<Self> {
        command.process_group(0);
        let child = command.kill_on_drop(true).spawn()?;
        Ok(Self {
            group: child.id(),
            child,
        })
    }

    /// Waits up to `grace` for the child to exit on its own, then SIGTERMs the group, waits up to
    /// `cleanup_timeout`, SIGKILLs it and reaps the child. Returns the exit observed within
    /// `grace`, or `None` when the child had to be signalled.
    async fn shutdown(
        &mut self,
        grace: Duration,
        cleanup_timeout: Duration,
    ) -> Option<std::io::Result<std::process::ExitStatus>> {
        let child = &mut self.child;
        let exited = tokio::time::timeout(grace, child.wait()).await.ok();
        if let Some(pid) = self.group {
            // The child is the process-group leader; signal the group so browser descendants
            // receive the same bounded graceful-then-forced cleanup.
            unsafe { libc::kill(-(pid as i32), libc::SIGTERM) };
            let _ = tokio::time::timeout(cleanup_timeout, child.wait()).await;
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
            let _ = child.wait().await;
            self.group = None;
        }
        exited
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        // Unawaited drops happen only while the leader is unreaped, so its group ID is not reused.
        if let Some(pid) = self.group.take() {
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        }
    }
}
fn encode_line(command: &WorkerCommand) -> Result<Vec<u8>> {
    let mut line = serde_json::to_vec(command)?;
    if line.len() + 1 > MAX_LINE {
        return Err(Error::Protocol("worker command exceeds 1 MiB".into()));
    }
    line.push(b'\n');
    Ok(line)
}
async fn send_line(w: &mut (impl AsyncWrite + Unpin), command: &WorkerCommand) -> Result<()> {
    let line = encode_line(command)?;
    w.write_all(&line).await?;
    w.flush().await?;
    Ok(())
}
async fn read_event(r: &mut (impl AsyncBufRead + Unpin)) -> Result<WorkerEvent> {
    let mut line = Vec::new();
    let n = r
        .take((MAX_LINE + 1) as u64)
        .read_until(b'\n', &mut line)
        .await?;
    if n == 0 || n > MAX_LINE || line.last() != Some(&b'\n') {
        return Err(Error::Protocol("invalid or overlong worker frame".into()));
    }
    serde_json::from_slice(&line).map_err(|e| Error::Protocol(e.to_string()))
}
async fn read_matching_terminal(
    r: &mut (impl AsyncBufRead + Unpin),
    job: &PublishJob,
    deadline: Instant,
) -> Result<WorkerEvent> {
    let event = tokio::time::timeout_at(deadline, read_event(r))
        .await
        .map_err(|_| Error::Protocol("worker artwork timeout".into()))??;
    match &event {
        WorkerEvent::Scheduled {
            protocol,
            attempt_id,
            uuid,
            ..
        }
        | WorkerEvent::Failed {
            protocol,
            attempt_id,
            uuid,
            ..
        }
        | WorkerEvent::Unknown {
            protocol,
            attempt_id,
            uuid,
            ..
        } if *protocol == 1 && attempt_id == &job.attempt_id && uuid == &job.uuid => Ok(event),
        _ => Err(Error::Protocol(
            "worker terminal identity/version mismatch".into(),
        )),
    }
}
use chrono::{Datelike, Timelike};

#[cfg(test)]
mod approved_bytes_tests {
    use super::{timeout_tests::publisher_fixture, *};
    use crate::images::file_sha512;
    use serde_json::json;

    fn prepared(root: &Path, store: &Store, selected: &[Value]) -> PreparedBatch {
        std::fs::create_dir_all(root.join(".publicator")).unwrap();
        prepare_batch(store, selected.iter().collect(), chrono_tz::UTC).unwrap()
    }

    #[test]
    fn preparation_derives_snapshot_and_upload_from_the_current_reference() {
        let (root, app) = publisher_fixture(1, "Description");
        let store = app.store();
        let selected = store.read().unwrap();
        let replacement = root.path().join("b.png");
        crate::image_fixture::solid_image(&replacement, "blue", 2, 2);
        let hash = file_sha512(&replacement).unwrap();
        store
            .mutate(|rows| {
                rows[0]["files"] = json!([{"basename": "b.png", "sha512sum": hash}]);
                Ok(())
            })
            .unwrap();

        let batch = prepared(root.path(), store, &selected);
        let (approved, job) = &batch.jobs[0];
        assert_eq!(approved.source_hash, hash);
        assert_eq!(file_sha512(&job.image_path).unwrap(), hash);
        assert!(job.image_path.starts_with(root.path().join(".publicator")));
    }

    #[test]
    fn dispatch_rejects_a_delivery_recorded_since_approval_but_recovery_applies_it() {
        let (root, app) = publisher_fixture(1, "Description");
        let store = app.store();
        let batch = prepared(root.path(), store, &store.read().unwrap());
        let (approved, job) = &batch.jobs[0];
        let cancel = AtomicBool::new(false);
        assert!(approved_for_dispatch(store, approved, job, &cancel).unwrap());
        store
            .mutate(|rows| {
                rows[0]["apparitions"][0]["state"] = json!("published_or_scheduled");
                rows[0]["state"] = json!("APPEARED");
                Ok(())
            })
            .unwrap();
        assert!(!approved_for_dispatch(store, approved, job, &cancel).unwrap());

        let journal = PublishJournal::new(root.path()).unwrap();
        journal.begin(job, approved).unwrap();
        journal
            .terminal(&WorkerEvent::Scheduled {
                protocol: 1,
                attempt_id: job.attempt_id.clone(),
                uuid: job.uuid.clone(),
                url: None,
            })
            .unwrap();
        assert_eq!(
            journal.recover(store).unwrap().replayed,
            std::slice::from_ref(&job.attempt_id)
        );
    }
}

#[cfg(test)]
pub(crate) mod timeout_tests {
    use super::*;
    use crate::{
        config::{Config, Paths},
        metadata::{Endpoints, MetadataClient},
        process_fixture::{eventually, running},
    };
    use serde_json::json;
    use std::sync::Arc;

    pub(crate) fn publisher_fixture(count: usize, description: &str) -> (TempDir, Application) {
        let root = tempfile::tempdir().unwrap();
        let mut rows = Vec::new();
        for n in 1..=count {
            let basename = format!("{n}.png");
            let image = root.path().join(&basename);
            crate::image_fixture::solid_image(&image, "white", 4, 3);
            let hash = crate::images::file_sha512(&image).unwrap();
            rows.push(json!({
                "uuid": format!("00000000-0000-4000-8000-{n:012}"),
                "state": "DESCRIBED",
                "title": "Title",
                "description": description,
                "submissionTimestamp": 1_700_002_800,
                "apparitionTimestampIfDifferentThanSubmission": 1_700_002_800,
                "files": [{"basename": basename, "sha512sum": hash}],
                "apparitions": [{"platformName": "deviantart", "state": "unpublished", "tiers": [], "galleries": []}]
            }));
        }
        std::fs::write(
            root.path().join("publications.json"),
            serde_json::to_vec(&rows).unwrap(),
        )
        .unwrap();
        std::fs::write(root.path().join("tags.txt"), "tag\n").unwrap();
        let store = crate::store::Store::new(
            Paths::resolve(Some(root.path()), None).unwrap(),
            Config {
                tags: Some("tags.txt".into()),
                ..Config::default()
            },
        )
        .unwrap();
        let app = Application::new(
            Arc::new(store),
            MetadataClient::new(
                reqwest::Client::new(),
                Endpoints {
                    openrouter: "http://127.0.0.1:1".into(),
                    ollama: "http://127.0.0.1:1".into(),
                },
                "llmfit".into(),
            ),
        );
        (root, app)
    }

    fn write_worker(root: &Path, script: &str) -> PathBuf {
        let path = root.join("worker.sh");
        crate::executable_fixture::write_executable(&path, script);
        path
    }

    const READY: &str = "IFS= read -r open || exit 1\necho '{\"protocol\":1,\"type\":\"ready\"}'";

    fn request() -> PublishRequest {
        PublishRequest {
            uuids: None,
            headless: true,
        }
    }

    #[tokio::test]
    async fn worker_launch_binds_store_data_root_outside_ndjson() {
        let root = tempfile::tempdir().unwrap();
        let worker = write_worker(root.path(), "#!/bin/sh\nprintf '%s\\n' \"$@\"\n");

        let mut process = start_worker(&worker, root.path()).unwrap();
        let mut stdout = String::new();
        process
            .child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .await
            .unwrap();
        assert!(process.child.wait().await.unwrap().success());
        assert_eq!(stdout, format!("--data-root\n{}\n", root.path().display()));
    }

    #[tokio::test]
    async fn cleanup_force_kills_worker_that_ignores_termination() {
        let (root, application) = publisher_fixture(1, "Description");
        let pid_file = root.path().join("pid");
        let _watchdog = KillRecorded(pid_file.clone());
        let worker = write_worker(
            root.path(),
            &format!(
                r#"#!/bin/sh
[ "$1" = "--check-steps" ] && exit 0
echo $$ > '{}'
while IFS= read -r line; do
case "$line" in
*'"type":"open"'*) echo '{{"protocol":1,"type":"ready"}}' ;;
*'"type":"submit"'*) printf '%s' "$line" | jq -c '{{protocol:1,type:"failed",attempt_id,uuid,error:"safe"}}'; trap '' TERM; while :; do :; done ;;
esac
done"#,
                pid_file.display()
            ),
        );

        let publisher =
            Publisher::new(worker, root.path().join("skill.md")).with_timeouts(Timeouts {
                ready: Duration::from_millis(250),
                artwork: Duration::from_millis(250),
                cleanup: Duration::from_millis(30),
                ..Timeouts::default()
            });
        // The spinning worker never reads `close`: waiting for its exit is bounded too.
        let report = tokio::time::timeout(
            Duration::from_secs(10),
            publisher.publish(&application, request(), Arc::default()),
        )
        .await
        .expect("cleanup deadlines bound a worker that ignores close and SIGTERM")
        .unwrap();
        assert_eq!(report.failed, 1);
        let pid = std::fs::read_to_string(pid_file).unwrap();
        assert!(std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())).is_err());
    }

    #[tokio::test]
    async fn worker_that_never_becomes_ready_obeys_ready_deadline() {
        let (root, application) = publisher_fixture(1, "Description");
        let (worker, watchdog) = hanging_worker(root.path(), "IFS= read -r open || exit 1");

        // Long enough for the worker to record its PIDs: nothing it sends can signal that.
        let publisher =
            Publisher::new(worker, root.path().join("skill.md")).with_timeouts(Timeouts {
                ready: Duration::from_secs(1),
                artwork: Duration::from_millis(250),
                cleanup: Duration::from_millis(50),
                ..Timeouts::default()
            });
        let error = publisher
            .publish(&application, request(), Arc::new(AtomicBool::new(false)))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("worker ready timeout"));
        assert_reaped(&watchdog.0);
    }

    #[tokio::test]
    async fn terminal_read_timeout_keeps_attempt_unknown_and_blocks_retry() {
        let (root, application) = publisher_fixture(1, "Description");
        let (worker, watchdog) = hanging_worker(
            root.path(),
            &format!("{READY}\nIFS= read -r submit || exit 1"),
        );

        let publisher =
            Publisher::new(worker, root.path().join("skill.md")).with_timeouts(Timeouts {
                ready: Duration::from_millis(250),
                artwork: Duration::from_millis(50),
                cleanup: Duration::from_millis(50),
                ..Timeouts::default()
            });
        let report = publisher
            .publish(&application, request(), Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(report.unknown, 1);
        assert!(report.error.unwrap().contains("artwork timeout"));
        assert_reaped(&watchdog.0);
        let retry = publisher
            .publish(&application, request(), Arc::new(AtomicBool::new(false)))
            .await;
        assert!(matches!(retry, Err(Error::ReconciliationRequired(_))));
    }

    #[tokio::test]
    async fn large_submit_to_nonreading_worker_times_out_with_unknown_intent() {
        let (root, application) = publisher_fixture(1, &"d".repeat(900_000));
        let (worker, watchdog) = hanging_worker(root.path(), READY);

        let publisher =
            Publisher::new(worker, root.path().join("skill.md")).with_timeouts(Timeouts {
                ready: Duration::from_millis(250),
                artwork: Duration::from_millis(50),
                cleanup: Duration::from_millis(100),
                ..Timeouts::default()
            });
        let report = publisher
            .publish(&application, request(), Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(report.unknown, 1);
        assert!(report.error.unwrap().contains("write timeout"));
        assert_reaped(&watchdog.0);
    }

    fn publishing_boundary_fixture() -> (
        TempDir,
        Application,
        crate::store::Store,
        Publisher,
        PathBuf,
    ) {
        let (root, app) = publisher_fixture(2, "Description");
        let store = app.store().clone();
        let calls = root.path().join("calls.log");
        let worker = write_worker(
            root.path(),
            &format!(
                r#"#!/bin/sh
[ "$1" = "--check-steps" ] && exit 0
while IFS= read -r line; do
  kind=$(printf '%s' "$line" | jq -r .type)
  case "$kind" in
    open) echo '{{"protocol":1,"type":"ready"}}' ;;
    submit)
      uuid=$(printf '%s' "$line" | jq -r .uuid)
      attempt=$(printf '%s' "$line" | jq -r .attempt_id)
      echo "$uuid" >> '{}'
      printf 'simulated worker stderr\n' >&2
      printf '{{"protocol":1,"type":"scheduled","attempt_id":"%s","uuid":"%s","url":"https://example.invalid/work"}}\n' "$attempt" "$uuid"
      ;;
    close) exit 0 ;;
  esac
done"#,
                calls.display()
            ),
        );

        let publisher = Publisher::new(worker, root.path().join("skill.md"));
        (root, app, store, publisher, calls)
    }

    fn recover_scheduled_receipt(root: &Path, store: &Store) -> PathBuf {
        let journal = PublishJournal::new(root).unwrap();
        let attempt = std::fs::read_dir(root.join(".publicator/publish"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .file_name()
            .into_string()
            .unwrap();
        let receipt_path = root
            .join(".publicator/publish")
            .join(&attempt)
            .join("receipt.json");
        let receipt: Value =
            serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
        assert_eq!(receipt["terminal"]["type"], "scheduled");
        assert_eq!(receipt["applied"], false);
        assert_eq!(journal.recover(store).unwrap().replayed, vec![attempt]);
        receipt_path
    }

    #[tokio::test]
    async fn pre_rename_failure_with_worker_stderr_remains_a_safe_io_error() {
        let (root, app, store, publisher, calls) = publishing_boundary_fixture();
        crate::store::fail_next_commit_for_test(crate::store::CommitFailureForTest::BeforeRename);
        let error = publisher
            .publish(&app, request(), Arc::new(AtomicBool::new(false)))
            .await
            .unwrap_err();
        let displayed = error.to_string();
        match error {
            Error::Io(source) => assert_eq!(source.to_string(), "injected pre-rename failure"),
            other => panic!("expected safe pre-rename I/O failure, got {other:?}"),
        }
        assert!(!displayed.contains("may be visible but is not confirmed durable"));
        assert_eq!(
            std::fs::read_to_string(calls)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            ["00000000-0000-4000-8000-000000000001"]
        );
        assert_eq!(
            store.read().unwrap()[0]["apparitions"][0]["state"],
            "unpublished"
        );
        recover_scheduled_receipt(root.path(), &store);
        assert_eq!(
            store.read().unwrap()[0]["apparitions"][0]["state"],
            "published_or_scheduled"
        );
    }

    #[tokio::test]
    async fn post_rename_failure_with_worker_stderr_keeps_typed_recoverable_receipt() {
        let (root, app, store, publisher, calls) = publishing_boundary_fixture();
        crate::store::fail_next_commit_for_test(crate::store::CommitFailureForTest::AfterRename);
        let error = publisher
            .publish(&app, request(), Arc::new(AtomicBool::new(false)))
            .await
            .unwrap_err();
        let displayed = error.to_string();
        match error {
            Error::DurabilityUncertain { path, source } => {
                assert_eq!(path, store.paths().json);
                assert_eq!(
                    source.to_string(),
                    "injected post-rename directory sync failure"
                );
            }
            other => panic!("expected typed uncertain commit, got {other:?}"),
        }
        assert!(displayed.contains("may be visible but is not confirmed durable"));
        assert_eq!(
            std::fs::read_to_string(calls)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            ["00000000-0000-4000-8000-000000000001"]
        );

        let fresh_rows = store.read().unwrap();
        assert_eq!(
            fresh_rows[0]["apparitions"][0]["state"],
            "published_or_scheduled"
        );
        assert_eq!(
            fresh_rows[0]["apparitions"][0]["url"],
            "https://example.invalid/work"
        );
        assert_eq!(fresh_rows[1]["apparitions"][0]["state"], "unpublished");

        let receipt_path = recover_scheduled_receipt(root.path(), &store);
        let receipt: Value =
            serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
        assert_eq!(receipt["applied"], true);
    }

    /// `fail_commit_for_test` arguments: the receipt write that follows `skip` successful ones
    /// (0 intent, 1 terminal, 2 applied), or the next Store write.
    type Checkpoint = (&'static str, usize, crate::store::CommitFailureForTest);
    const RECEIPT: &str = "receipt.json";
    const STORE: &str = "publications.json";

    #[derive(Debug)]
    enum AfterReopen {
        Clean,
        Replayed,
        Blocked(crate::publish_journal::ReconcileOutcome),
    }

    fn sent(calls: &Path) -> Vec<String> {
        std::fs::read_to_string(calls)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The only attempt's ID and its durable receipt state.
    fn durable_receipt(root: &Path) -> (String, &'static str) {
        let mut attempts = std::fs::read_dir(root.join(".publicator/publish"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(attempts.len(), 1, "{attempts:?}");
        let attempt = attempts.pop().unwrap();
        let id = attempt.file_name().unwrap().to_string_lossy().into_owned();
        let Ok(bytes) = std::fs::read(attempt.join("receipt.json")) else {
            return (id, "absent");
        };
        let receipt: Value = serde_json::from_slice(&bytes).unwrap();
        let state = match (receipt["terminal"]["type"].as_str(), &receipt["applied"]) {
            (None, Value::Bool(false)) => "pending",
            (Some("scheduled"), Value::Bool(false)) => "scheduled",
            (Some("scheduled"), Value::Bool(true)) => "applied",
            other => panic!("unexpected receipt state {other:?}"),
        };
        (id, state)
    }

    async fn assert_checkpoint(checkpoint: Checkpoint, receipt: &str, after: AfterReopen) {
        let (root, app, store, publisher, calls) = publishing_boundary_fixture();
        let first = vec!["00000000-0000-4000-8000-000000000001".to_owned()];
        let publish = || publisher.publish(&app, request(), Arc::new(AtomicBool::new(false)));
        let (file, skip, failure) = checkpoint;
        crate::store::fail_commit_for_test(file, skip, failure);
        assert!(publish().await.is_err(), "{checkpoint:?}");
        let expected_sent = if (file, skip) == (RECEIPT, 0) {
            Vec::new()
        } else {
            first
        };
        assert_eq!(sent(&calls), expected_sent, "{checkpoint:?} next send");
        let (attempt, state) = durable_receipt(root.path());
        assert_eq!(state, receipt, "{checkpoint:?} durable receipt");

        let journal = PublishJournal::new(root.path()).unwrap();
        let recovery = journal.recover(&store).unwrap();
        match after {
            AfterReopen::Clean => assert_eq!(recovery, Default::default(), "{checkpoint:?}"),
            AfterReopen::Replayed => {
                assert_eq!(recovery.replayed, [attempt], "{checkpoint:?}");
                assert!(recovery.blocked.is_empty(), "{checkpoint:?}");
            }
            AfterReopen::Blocked(decision) => {
                assert_eq!(recovery.blocked.len(), 1, "{checkpoint:?}");
                let receipt_path = root.path().join(".publicator/publish").join(&attempt);
                let bytes = std::fs::read(receipt_path.join("receipt.json")).unwrap();
                assert!(matches!(
                    publish().await,
                    Err(Error::ReconciliationRequired(_))
                ));
                assert_eq!(sent(&calls), expected_sent, "{checkpoint:?} blocked resend");
                assert_eq!(
                    std::fs::read(receipt_path.join("receipt.json")).unwrap(),
                    bytes
                );
                journal.reconcile(&store, &attempt, decision).unwrap();
            }
        }
        let retry = publish().await.unwrap();
        assert!(retry.error.is_none(), "{checkpoint:?}: {retry:?}");
        assert_eq!(
            sent(&calls),
            [
                "00000000-0000-4000-8000-000000000001",
                "00000000-0000-4000-8000-000000000002"
            ],
            "{checkpoint:?} exactly-once sends"
        );
        for row in store.read().unwrap() {
            assert_eq!(row["apparitions"][0]["state"], "published_or_scheduled");
        }
    }

    #[tokio::test]
    async fn receipt_and_store_checkpoint_failures_never_resend_and_recover_exactly_once() {
        use crate::{
            publish_journal::ReconcileOutcome,
            store::CommitFailureForTest::{AfterRename, BeforeRename},
        };
        use AfterReopen::{Blocked, Clean, Replayed};
        let scheduled = ReconcileOutcome::Scheduled {
            actual_url: Some("https://example.invalid/work".into()),
        };
        let cases = [
            ((RECEIPT, 0, BeforeRename), "absent", Clean),
            (
                (RECEIPT, 0, AfterRename),
                "pending",
                Blocked(ReconcileOutcome::NotSubmitted),
            ),
            ((RECEIPT, 1, BeforeRename), "pending", Blocked(scheduled)),
            ((RECEIPT, 1, AfterRename), "scheduled", Replayed),
            ((STORE, 0, BeforeRename), "scheduled", Replayed),
            ((STORE, 0, AfterRename), "scheduled", Replayed),
            ((RECEIPT, 2, BeforeRename), "scheduled", Replayed),
            ((RECEIPT, 2, AfterRename), "applied", Clean),
        ];
        for (checkpoint, receipt, after) in cases {
            assert_checkpoint(checkpoint, receipt, after).await;
        }
    }

    fn recorded(gate: &Path) -> Vec<i32> {
        std::fs::read_to_string(gate)
            .unwrap_or_default()
            .split_whitespace()
            .filter_map(|pid| pid.parse().ok())
            .collect()
    }

    /// SIGKILLs the PIDs recorded in a gate file if the test fails before the publisher does.
    struct KillRecorded(PathBuf);

    impl Drop for KillRecorded {
        fn drop(&mut self) {
            for pid in recorded(&self.0).into_iter().filter(|&pid| running(pid)) {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }

    /// Shell that starts a same-group `sleep` and atomically records "<pid> <child pid>" in `gate`.
    fn hold_recorded_child(gate: &Path) -> String {
        format!(
            r#"sleep 1000 & echo "$$ $!" > '{gate}.tmp'; mv '{gate}.tmp' '{gate}'"#,
            gate = gate.display()
        )
    }

    /// Worker that first records its held child, then runs `protocol` and waits forever;
    /// the returned guard kills what it recorded.
    fn hanging_worker(root: &Path, protocol: &str) -> (PathBuf, KillRecorded) {
        let gate = root.join("worker-gate");
        let script = format!(
            "#!/bin/sh\n[ \"$1\" = \"--check-steps\" ] && exit 0\n{}\n{protocol}\nwait\n",
            hold_recorded_child(&gate)
        );
        (write_worker(root, &script), KillRecorded(gate))
    }

    /// Both PIDs recorded in `gate` are gone once the publisher returns.
    fn assert_reaped(gate: &Path) {
        let pids = recorded(gate);
        assert_eq!(pids.len(), 2, "{pids:?}");
        assert!(
            eventually(Duration::from_secs(5), || !pids
                .iter()
                .any(|&pid| running(pid))),
            "fixture tree survived: {pids:?}"
        );
    }

    /// Step checker that hangs with a same-group child after recording "<pid> <child pid>".
    fn hanging_checker(root: &Path) -> (Publisher, PathBuf, PathBuf) {
        let (gate, started) = (root.join("checker"), root.join("started"));
        let worker = write_worker(
            root,
            &format!(
                r#"#!/bin/sh
if [ "$1" = "--check-steps" ]; then
  {held}; wait
fi
touch '{started}'"#,
                held = hold_recorded_child(&gate),
                started = started.display()
            ),
        );
        (Publisher::new(worker, root.join("skill.md")), gate, started)
    }

    #[tokio::test]
    async fn hanging_step_check_is_killed_with_its_children_and_releases_the_publisher() {
        let (root, application) = publisher_fixture(1, "Description");
        let (publisher, gate, started) = hanging_checker(root.path());
        let _watchdog = KillRecorded(gate.clone());
        let publisher = publisher.with_timeouts(Timeouts {
            check_steps: Duration::from_secs(1),
            cleanup: Duration::from_millis(50),
            ..Timeouts::default()
        });
        // The second run proves the lock was released: it times out again instead of Busy.
        for _ in 0..2 {
            let _ = std::fs::remove_file(&gate);
            let error = tokio::time::timeout(
                Duration::from_secs(10),
                publisher.publish(&application, request(), Arc::default()),
            )
            .await
            .expect("step check deadline bounds the publish")
            .unwrap_err();
            assert!(
                error.to_string().contains("step check timed out"),
                "{error}"
            );
            assert_reaped(&gate);
            assert!(!started.exists());
            drop(publish_journal::publisher_lock(root.path()).unwrap());
        }
    }

    #[tokio::test]
    async fn dropping_a_publish_during_the_step_check_kills_the_checker_group() {
        let (root, application) = publisher_fixture(1, "Description");
        let (publisher, gate, started) = hanging_checker(root.path());
        let _watchdog = KillRecorded(gate.clone());
        let mut publish = Box::pin(publisher.publish(&application, request(), Arc::default()));
        let gated = tokio::time::timeout(Duration::from_secs(10), async {
            while !gate.exists() {
                tokio::select! {
                    result = &mut publish => panic!("publish ended during the step check: {result:?}"),
                    () = tokio::time::sleep(Duration::from_millis(10)) => {}
                }
            }
        })
        .await;
        assert!(gated.is_ok(), "checker never started");
        drop(publish);
        assert_reaped(&gate);
        assert!(!started.exists());
        drop(publish_journal::publisher_lock(root.path()).unwrap());
    }

    #[tokio::test]
    async fn detached_stderr_holder_cannot_stall_teardown_or_lose_diagnostics() {
        let (root, application) = publisher_fixture(1, "Description");
        let holder = root.path().join("holder");
        let _watchdog = KillRecorded(holder.clone());
        let worker = write_worker(
            root.path(),
            &format!(
                r#"#!/bin/sh
[ "$1" = "--check-steps" ] && exit 0
while IFS= read -r line; do
case "$line" in
*'"type":"open"'*) echo '{{"protocol":1,"type":"ready"}}' ;;
*'"type":"submit"'*) echo 'stderr before detaching' >&2
  python3 -c 'import os, sys, time; os.setsid(); open(sys.argv[1] + ".tmp", "w").write(str(os.getpid())); os.rename(sys.argv[1] + ".tmp", sys.argv[1]); time.sleep(1000)' '{holder}' >/dev/null &
  while [ ! -e '{holder}' ]; do sleep 0.01; done
  printf '%s' "$line" | jq -c '{{protocol:1,type:"failed",attempt_id,uuid,error:"safe"}}' ;;
*'"type":"close"'*) exit 0 ;;
esac
done"#,
                holder = holder.display()
            ),
        );
        let publisher =
            Publisher::new(worker, root.path().join("skill.md")).with_timeouts(Timeouts {
                cleanup: Duration::from_millis(100),
                ..Timeouts::default()
            });
        let report = tokio::time::timeout(
            Duration::from_secs(10),
            publisher.publish(&application, request(), Arc::default()),
        )
        .await
        .expect("a detached stderr holder must not stall teardown")
        .unwrap();
        assert_eq!(report.failed, 1);
        let error = report.error.unwrap();
        assert!(
            error.contains("worker stderr: stderr before detaching"),
            "{error}"
        );
        let pids = recorded(&holder);
        assert!(
            pids.len() == 1 && running(pids[0]),
            "holder must outlive the worker group: {pids:?}"
        );
    }
}
