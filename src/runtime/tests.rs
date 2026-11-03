use super::*;
use std::sync::atomic::AtomicUsize;

#[derive(Default)]
struct Observed {
    runs: AtomicUsize,
    dropped: AtomicBool,
    releases: Mutex<Vec<(String, std::thread::ThreadId)>>,
    worker: Mutex<Option<std::thread::ThreadId>>,
}
struct FakeBackend {
    observed: Arc<Observed>,
    next_id: u64,
    objects: BTreeMap<String, u64>,
    started: Option<oneshot::Sender<()>>,
    gate: mpsc::Receiver<()>,
}
impl FakeBackend {
    fn create(&mut self) -> String {
        self.next_id += 1;
        let handle = format!("fixture:{}", self.next_id);
        self.objects.insert(handle.clone(), self.next_id);
        handle
    }
}
impl Backend for FakeBackend {
    fn run(&mut self, action: Action, trace: &Trace) -> Result<Value> {
        self.observed.runs.fetch_add(1, Ordering::SeqCst);
        match action {
            Action::Objects => Ok(json!({"objects":self.objects.keys().collect::<Vec<_>>()})),
            Action::Create { class_id } => {
                let handle = self.create();
                if class_id == "blocked" {
                    trace.run_phase("fixture_open", || {
                        self.started.take().unwrap().send(()).unwrap();
                        self.gate.recv().unwrap();
                        Ok(())
                    })?;
                }
                Ok(json!({"$object":handle}))
            }
            Action::SessionStart { .. } => {
                self.create(); // Broker created, then opening partially succeeds.
                self.create();
                Err(Error::new("fixture_open_failed", "Opening failed"))
            }
            Action::Reset => {
                let n = self.release_handles(&self.handles_since(0));
                Ok(json!({"released_objects":n}))
            }
            _ => unreachable!(),
        }
    }
    fn checkpoint(&self) -> u64 {
        self.next_id
    }
    fn handles_since(&self, checkpoint: u64) -> Vec<String> {
        self.objects
            .iter()
            .filter(|(_, serial)| **serial > checkpoint)
            .map(|(h, _)| h.clone())
            .collect()
    }
    fn release_handles(&mut self, handles: &[String]) -> usize {
        let mut handles = handles.to_vec();
        handles.sort_by_key(|h| std::cmp::Reverse(self.objects.get(h).copied().unwrap_or(0)));
        let mut released = 0;
        for handle in handles {
            if self.objects.remove(&handle).is_some() {
                released += 1;
                self.observed
                    .releases
                    .lock()
                    .unwrap()
                    .push((handle, std::thread::current().id()));
            }
        }
        released
    }
}
impl Drop for FakeBackend {
    fn drop(&mut self) {
        self.release_handles(&self.handles_since(0));
        self.observed.dropped.store(true, Ordering::SeqCst);
    }
}
fn fixture() -> (
    Runtime,
    Arc<Observed>,
    oneshot::Receiver<()>,
    mpsc::Sender<()>,
) {
    let observed = Arc::new(Observed::default());
    let worker_observed = Arc::clone(&observed);
    let (started_tx, started_rx) = oneshot::channel();
    let (gate_tx, gate_rx) = mpsc::channel();
    let runtime = Runtime::start_worker(
        move || {
            *worker_observed.worker.lock().unwrap() = Some(std::thread::current().id());
            Ok(FakeBackend {
                observed: worker_observed,
                next_id: 0,
                objects: BTreeMap::new(),
                started: Some(started_tx),
                gate: gate_rx,
            })
        },
        Duration::from_millis(5),
    )
    .unwrap();
    (runtime, observed, started_rx, gate_tx)
}
fn create(name: &str) -> Action {
    Action::Create {
        class_id: name.into(),
    }
}

#[tokio::test]
async fn failed_session_releases_partial_references_but_preserves_existing_handles() {
    let (runtime, observed, _, _) = fixture();
    let old = runtime.execute(create("existing")).await.unwrap()["$object"].clone();
    let error = runtime
        .execute(Action::SessionStart { file_path: None })
        .await
        .unwrap_err();
    assert_eq!(error.code, "fixture_open_failed");
    assert_eq!(
        runtime.execute(Action::Objects).await.unwrap()["objects"],
        json!([old])
    );
    let releases = observed.releases.lock().unwrap().clone();
    assert_eq!(
        releases.iter().map(|(h, _)| h.as_str()).collect::<Vec<_>>(),
        vec!["fixture:3", "fixture:2"]
    );
    runtime.shutdown().await.unwrap();
    let worker = observed.worker.lock().unwrap().unwrap();
    assert!(
        observed
            .releases
            .lock()
            .unwrap()
            .iter()
            .all(|(_, id)| *id == worker)
    );
    assert!(observed.dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn cancelling_an_active_creation_cleans_orphaned_handles_on_the_sta() {
    let (runtime, observed, started, gate) = fixture();
    let task_runtime = runtime.clone();
    let task = tokio::spawn(async move { task_runtime.execute(create("blocked")).await });
    started.await.unwrap();
    // status must respond without waiting for the blocked backend.
    assert_eq!(
        runtime.status()["active_operation"]["phase"],
        "fixture_open"
    );
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    gate.send(()).unwrap();
    assert_eq!(
        runtime.execute(Action::Objects).await.unwrap()["objects"],
        json!([])
    );
    runtime.shutdown().await.unwrap();
    assert_eq!(observed.releases.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn dropping_a_delivered_but_unaccepted_result_does_not_leak_handles() {
    let (runtime, observed, _, _) = fixture();
    let (receiver, _) = runtime.submit(create("unaccepted")).unwrap();
    let delivery = receiver.await.unwrap();
    // oneshot send succeeded, but Runtime::execute never accepted the result.
    drop(delivery);
    assert_eq!(
        runtime.execute(Action::Objects).await.unwrap()["objects"],
        json!([])
    );
    runtime.shutdown().await.unwrap();
    assert_eq!(observed.releases.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cancelled_queued_work_is_never_executed() {
    let (runtime, observed, started, gate) = fixture();
    let (blocked, _) = runtime.submit(create("blocked")).unwrap();
    started.await.unwrap();
    let (cancelled, _) = runtime.submit(create("must_not_run")).unwrap();
    drop(cancelled);
    gate.send(()).unwrap();
    blocked.await.unwrap().accept().unwrap();
    let objects = runtime.execute(Action::Objects).await.unwrap();
    assert_eq!(objects["objects"].as_array().unwrap().len(), 1);
    assert_eq!(observed.runs.load(Ordering::SeqCst), 2); // create + objects.
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_waits_for_active_call_skips_queue_and_survives_waiter_cancellation() {
    let (runtime, observed, started, gate) = fixture();
    let (blocked, _) = runtime.submit(create("blocked")).unwrap();
    started.await.unwrap();
    let (queued, _) = runtime.submit(create("must_not_run")).unwrap();
    // Poll shutdown until it has started joining, then cancel only that waiter.
    let shutdown_runtime = runtime.clone();
    let mut shutdown = Box::pin(shutdown_runtime.shutdown());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut shutdown)
            .await
            .is_err()
    );
    drop(shutdown);
    assert!(!observed.dropped.load(Ordering::SeqCst));
    assert_eq!(
        runtime.execute(Action::Objects).await.unwrap_err().code,
        "runtime_shutting_down"
    );
    gate.send(()).unwrap();
    blocked.await.unwrap().accept().unwrap();
    assert_eq!(
        queued.await.unwrap().accept().unwrap_err().code,
        "runtime_shutting_down"
    );
    let (first, second) = tokio::join!(runtime.shutdown(), runtime.shutdown());
    first.unwrap();
    second.unwrap();
    assert_eq!(observed.runs.load(Ordering::SeqCst), 1);
    assert!(observed.dropped.load(Ordering::SeqCst));
    assert_eq!(runtime.status()["worker_running"], false);
    assert_eq!(runtime.status()["active_operation"], Value::Null);
}

#[tokio::test]
async fn worker_initialization_failure_still_allows_orderly_shutdown() {
    let runtime = Runtime::start_worker::<FakeBackend>(
        || Err(Error::new("fixture_init_failed", "initialization failed")),
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(
        runtime.execute(Action::Objects).await.unwrap_err().code,
        "fixture_init_failed"
    );
    runtime.shutdown().await.unwrap();
    assert_eq!(runtime.status()["worker_running"], false);
}
