//! The COM runtime: one single-threaded apartment (STA) worker thread that owns
//! every COM object, fed through a bounded queue.
//!
//! Only plain Rust/JSON values cross this boundary. COM objects stay on the STA.
use crate::{
    catalog::{Catalog, Operation},
    error::{Error, Result},
    policy::ExecutionPolicy,
};
use diagnostics::{Trace, emit};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};
use tokio::sync::oneshot;
mod diagnostics;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows;

/// A unit of work for the STA worker.
#[derive(Debug)]
pub enum Action {
    Probe,
    SessionStart {
        file_path: Option<String>,
    },
    ViewerOpen {
        handle: String,
    },
    Create {
        class_id: String,
    },
    Call {
        handle: String,
        operation: Box<Operation>,
        arguments: BTreeMap<String, Value>,
    },
    Query {
        handle: String,
        interface: String,
    },
    Release {
        handle: String,
    },
    Objects,
    Reset,
}
impl Action {
    fn label(&self) -> &'static str {
        match self {
            Self::Probe => "probe",
            Self::SessionStart { .. } => "session_start",
            Self::ViewerOpen { .. } => "viewer_open",
            Self::Create { .. } => "object_create",
            Self::Call { .. } => "api_call",
            Self::Query { .. } => "object_query",
            Self::Release { .. } => "object_release",
            Self::Objects => "objects",
            Self::Reset => "reset",
        }
    }
}

/// A caller can disappear while its result is still in the oneshot. Undisclosed
/// handles must then return to the STA; never release COM on the async thread.
struct Delivery {
    result: Option<Result<Value>>,
    handles: Vec<String>,
    cleanup: mpsc::Sender<Vec<String>>,
}
impl Delivery {
    fn accept(mut self) -> Result<Value> {
        self.handles.clear();
        self.result
            .take()
            .expect("a delivery holds its result until it is accepted once")
    }
}
impl Drop for Delivery {
    fn drop(&mut self) {
        if !self.handles.is_empty() {
            let _ = self.cleanup.send(std::mem::take(&mut self.handles));
        }
    }
}
struct Request {
    action: Action,
    trace: Arc<Trace>,
    reply: oneshot::Sender<Delivery>,
}
#[derive(Default)]
struct Status {
    closing: AtomicBool,
    worker_running: AtomicBool,
    queued: AtomicUsize,
    application_ready: AtomicBool,
    active: Mutex<Option<Arc<Trace>>>,
}
struct Inner {
    tx: Mutex<Option<mpsc::SyncSender<Request>>>,
    worker: tokio::sync::Mutex<WorkerJoin>,
    status: Arc<Status>,
    next_id: AtomicU64,
    slow_call_after: Duration,
}
struct WorkerJoin {
    thread: Option<JoinHandle<()>>,
    joining: Option<tokio::task::JoinHandle<std::thread::Result<()>>>,
    outcome: Option<Result<()>>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        // Fallback for library callers. The executable awaits shutdown() too,
        // keeping the process alive until the STA has released COM references.
        self.status.closing.store(true, Ordering::SeqCst);
        // A destructor must not panic: recover the sender even if poisoned.
        self.tx
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }
}
/// Cheap-to-clone handle to the STA worker.
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<Inner>,
    policy: ExecutionPolicy,
}
impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("policy", &self.policy)
            .field("slow_call_after", &self.inner.slow_call_after)
            .finish_non_exhaustive()
    }
}

/// Created and dropped on the worker. The backend need not be Send.
trait Backend {
    fn run(&mut self, action: Action, trace: &Trace) -> Result<Value>;
    fn checkpoint(&self) -> u64;
    fn handles_since(&self, checkpoint: u64) -> Vec<String>;
    fn release_handles(&mut self, handles: &[String]) -> usize;
    fn application_ready(&self) -> bool {
        false
    }
    fn pump(&self) {}
}
#[cfg(windows)]
struct WindowsBackend {
    state: windows::State,
    // Struct fields drop in declaration order: objects before CoUninitialize.
    _apartment: windows::Apartment,
}
#[cfg(windows)]
impl Backend for WindowsBackend {
    fn run(&mut self, action: Action, trace: &Trace) -> Result<Value> {
        self.state.run(action, trace)
    }
    fn checkpoint(&self) -> u64 {
        self.state.checkpoint()
    }
    fn handles_since(&self, checkpoint: u64) -> Vec<String> {
        self.state.handles_since(checkpoint)
    }
    fn release_handles(&mut self, handles: &[String]) -> usize {
        self.state.release_handles(handles)
    }
    fn pump(&self) {
        windows::pump_messages();
    }
    fn application_ready(&self) -> bool {
        self.state.application_ready()
    }
}
#[cfg(not(windows))]
struct UnsupportedBackend;
#[cfg(not(windows))]
impl Backend for UnsupportedBackend {
    fn run(&mut self, _: Action, _: &Trace) -> Result<Value> {
        Err(Error::new(
            "unsupported_platform",
            "COM automation requires Windows; the catalog works on every platform.",
        ))
    }
    fn checkpoint(&self) -> u64 {
        0
    }
    fn handles_since(&self, _: u64) -> Vec<String> {
        Vec::new()
    }
    fn release_handles(&mut self, _: &[String]) -> usize {
        0
    }
}

impl Runtime {
    pub fn start(catalog: Arc<Catalog>) -> Result<Self> {
        Self::start_with_options(catalog, Duration::from_secs(10))
    }
    pub fn start_with_options(catalog: Arc<Catalog>, slow_call_after: Duration) -> Result<Self> {
        let policy = ExecutionPolicy::from_env();
        #[cfg(windows)]
        {
            let mut runtime = Self::start_worker(
                move || {
                    let apartment = windows::Apartment::initialize()?;
                    Ok(WindowsBackend {
                        state: windows::State::with_policy(catalog, policy),
                        _apartment: apartment,
                    })
                },
                slow_call_after,
            )?;
            runtime.policy = policy;
            Ok(runtime)
        }
        #[cfg(not(windows))]
        {
            drop(catalog);
            let mut runtime = Self::start_worker(|| Ok(UnsupportedBackend), slow_call_after)?;
            runtime.policy = policy;
            Ok(runtime)
        }
    }
    fn start_worker<B: Backend + 'static>(
        factory: impl FnOnce() -> Result<B> + Send + 'static,
        slow_call_after: Duration,
    ) -> Result<Self> {
        if slow_call_after.is_zero() {
            return Err(Error::invalid("slow_call_after must be greater than zero"));
        }
        let (tx, rx) = mpsc::sync_channel::<Request>(128);
        let (cleanup_tx, cleanup_rx) = mpsc::channel();
        let status = Arc::new(Status::default());
        status.worker_running.store(true, Ordering::SeqCst);
        let worker_status = Arc::clone(&status);
        let worker = std::thread::Builder::new()
            .name("control-expert-sta".into())
            .spawn(move || {
                worker_loop(factory, &rx, &cleanup_tx, &cleanup_rx, &worker_status);
            })
            .map_err(|e| Error::new("runtime_start_failed", e.to_string()))?;
        Ok(Self {
            policy: ExecutionPolicy::default(),
            inner: Arc::new(Inner {
                tx: Mutex::new(Some(tx)),
                worker: tokio::sync::Mutex::new(WorkerJoin {
                    thread: Some(worker),
                    joining: None,
                    outcome: None,
                }),
                status,
                next_id: AtomicU64::new(0),
                slow_call_after,
            }),
        })
    }
    fn submit(&self, action: Action) -> Result<(oneshot::Receiver<Delivery>, Arc<Trace>)> {
        let sender = self.inner.tx.lock().expect("runtime sender lock poisoned");
        let sender = sender.as_ref().ok_or_else(closing_error)?;
        let api_id = match &action {
            Action::Call { operation, .. } => Some(operation.id.clone()),
            _ => None,
        };
        let trace = Arc::new(Trace::new(
            self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1,
            action.label(),
            api_id,
        ));
        let (reply, rx) = oneshot::channel();
        self.inner.status.queued.fetch_add(1, Ordering::SeqCst);
        if let Err(error) = sender.try_send(Request {
            action,
            trace: Arc::clone(&trace),
            reply,
        }) {
            self.inner.status.queued.fetch_sub(1, Ordering::SeqCst);
            return Err(Error::new(
                "runtime_unavailable",
                format!("Could not queue the operation: {error}"),
            ));
        }
        Ok((rx, trace))
    }
    pub async fn execute(&self, action: Action) -> Result<Value> {
        let (mut rx, trace) = self.submit(action)?;
        loop {
            tokio::select! {
                result = &mut rx => return result.map_err(|_| Error::new("runtime_unavailable", "The COM thread ended before answering"))?.accept(),
                () = tokio::time::sleep(self.inner.slow_call_after) => trace.event("operation_waiting"),
            }
        }
    }
    /// No STA roundtrip: remains available while a COM call is blocked.
    pub fn execution_policy(&self) -> Value {
        self.policy.snapshot()
    }
    pub fn operation_policy(&self, operation: &Operation) -> Value {
        self.policy.describe(operation)
    }
    pub fn status(&self) -> Value {
        let status = &self.inner.status;
        let running = status.worker_running.load(Ordering::SeqCst);
        let active = status
            .active
            .lock()
            .expect("active operation lock poisoned")
            .as_ref()
            .map(|t| t.snapshot());
        json!({
            "accepting_requests": !status.closing.load(Ordering::SeqCst) && running,
            "worker_running": running,
            "application_ready": running && status.application_ready.load(Ordering::SeqCst),
            "queued_operations": if running { status.queued.load(Ordering::SeqCst) } else { 0 },
            "active_operation": active,
            "slow_call_after_ms": self.inner.slow_call_after.as_millis(),
            "note": "application_ready means a validated and retained IApplication reference, not a continuous liveness check. A COM operation that started cannot be interrupted or retried automatically."
        })
    }
    /// Reject new work, skip queued requests, finish the active call, release
    /// references on the STA and join it before permitting process exit.
    pub async fn shutdown(&self) -> Result<()> {
        {
            let mut sender = self.inner.tx.lock().expect("runtime sender lock poisoned");
            self.inner.status.closing.store(true, Ordering::SeqCst);
            sender.take();
        }
        // Concurrent callers must all wait for the join to finish.
        let mut worker = self.inner.worker.lock().await;
        if let Some(outcome) = &worker.outcome {
            return outcome.clone();
        }
        if let Some(thread) = worker.thread.take() {
            emit("runtime_shutdown_started", self.status());
            worker.joining = Some(tokio::task::spawn_blocking(move || thread.join()));
        }
        // Await by reference: cancellation of this future leaves the join
        // task owned by Runtime, so another caller can still wait for it.
        let joined = loop {
            let joining = worker
                .joining
                .as_mut()
                .expect("the join task exists until an outcome is recorded");
            tokio::select! {
                joined = joining => break joined,
                () = tokio::time::sleep(self.inner.slow_call_after) => emit("runtime_shutdown_waiting", self.status()),
            }
        };
        let outcome = match joined {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(Error::new(
                "runtime_shutdown_failed",
                "The COM thread panicked",
            )),
            Err(error) => Err(Error::new("runtime_shutdown_failed", error.to_string())),
        };
        worker.joining.take();
        worker.outcome = Some(outcome.clone());
        emit("runtime_shutdown_finished", self.status());
        outcome
    }
}
fn closing_error() -> Error {
    Error::new(
        "runtime_shutting_down",
        "The runtime is shutting down; it accepts no new operations",
    )
}

/// Releases handles whose delivery was dropped, then refreshes the gate.
fn release_orphans<B: Backend>(
    state: &mut B,
    cleanup_rx: &mpsc::Receiver<Vec<String>>,
    status: &Status,
) {
    for handles in cleanup_rx.try_iter() {
        let released = state.release_handles(&handles);
        emit(
            "orphaned_references_released",
            json!({"released_objects":released}),
        );
    }
    status
        .application_ready
        .store(state.application_ready(), Ordering::SeqCst);
}

fn worker_loop<B: Backend>(
    factory: impl FnOnce() -> Result<B>,
    rx: &mpsc::Receiver<Request>,
    cleanup_tx: &mpsc::Sender<Vec<String>>,
    cleanup_rx: &mpsc::Receiver<Vec<String>>,
    status: &Arc<Status>,
) {
    struct WorkerExit(Arc<Status>);
    impl Drop for WorkerExit {
        fn drop(&mut self) {
            self.0.worker_running.store(false, Ordering::SeqCst);
            self.0.application_ready.store(false, Ordering::SeqCst);
            // A destructor must not panic: clear the slot even if poisoned.
            *self.0.active.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
    }
    let _exit = WorkerExit(Arc::clone(status));
    let mut backend = factory();
    loop {
        if let Ok(state) = &mut backend {
            release_orphans(state, cleanup_rx, status);
        }
        match rx.recv_timeout(Duration::from_millis(15)) {
            Ok(request) => {
                status.queued.fetch_sub(1, Ordering::SeqCst);
                // A delivery may have been dropped while recv_timeout waited.
                if let Ok(state) = &mut backend {
                    release_orphans(state, cleanup_rx, status);
                }
                if request.reply.is_closed() {
                    request.trace.event("operation_cancelled_before_execution");
                    continue;
                }
                *status
                    .active
                    .lock()
                    .expect("active operation lock poisoned") = Some(Arc::clone(&request.trace));
                request.trace.event("operation_started");
                let mut handles = Vec::new();
                let result = if status.closing.load(Ordering::SeqCst) {
                    Err(closing_error())
                } else {
                    match &mut backend {
                        Ok(state) => {
                            let checkpoint = state.checkpoint();
                            let result = request.trace.run_phase("executing", || {
                                state.run(request.action, &request.trace)
                            });
                            handles = state.handles_since(checkpoint);
                            if result.is_err() {
                                let released = state.release_handles(&handles);
                                handles.clear();
                                emit(
                                    "failed_operation_references_released",
                                    json!({"released_objects":released,"operation":request.trace.snapshot()}),
                                );
                            }
                            status
                                .application_ready
                                .store(state.application_ready(), Ordering::SeqCst);
                            result
                        }
                        Err(error) => Err(error.clone()),
                    }
                };
                request.trace.finish(&result);
                let _ = request.reply.send(Delivery {
                    result: Some(result),
                    handles,
                    cleanup: cleanup_tx.clone(),
                });
                *status
                    .active
                    .lock()
                    .expect("active operation lock poisoned") = None;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if let Ok(state) = &backend {
            state.pump();
        }
    }
    let remaining = backend
        .as_ref()
        .map_or(0, |state| state.handles_since(0).len());
    drop(backend); // Objects and then the COM apartment.
    emit(
        "com_worker_stopped",
        json!({"references_released":true,"released_objects":remaining}),
    );
}
