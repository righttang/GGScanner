//! In-process GUI scans use the same execution queue as HTTP jobs.
use super::*;

pub type LocalScanSlot = Arc<Mutex<Option<LocalScanClient>>>;
pub(super) type ActiveCancel = Arc<Mutex<Option<(String, yas::cancel::CancelToken)>>>;

#[derive(Clone)]
pub struct LocalScanClient {
    pub(super) tx: mpsc::Sender<(String, ExecutionRequest)>,
    pub(super) state: Arc<Mutex<JobState>>,
    pub(super) active: ActiveCancel,
    pub(super) shutdown: Arc<AtomicBool>,
}

pub(super) struct LocalScanTask {
    pub request: ScanRequest,
    pub run: Box<dyn FnOnce() -> Result<String> + Send>,
    pub token: yas::cancel::CancelToken,
    pub reply: mpsc::SyncSender<Result<String>>,
}

impl LocalScanClient {
    pub fn submit(
        &self,
        request: ScanRequest,
        token: yas::cancel::CancelToken,
        run: Box<dyn FnOnce() -> Result<String> + Send>,
    ) -> Result<mpsc::Receiver<Result<String>>> {
        let mut state = self.state.lock().unwrap();
        if self.shutdown.load(Ordering::Acquire) || state.state == JobPhase::Running {
            return Err(anyhow!("another task is running or the server is stopping"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        *state = JobState::running_scan(
            id.clone(),
            request.characters,
            request.weapons,
            request.artifacts,
            request.achievements,
        );
        *self.active.lock().unwrap() = Some((id.clone(), token.clone()));
        let (reply, done) = mpsc::sync_channel(1);
        if self
            .tx
            .send((
                id,
                ExecutionRequest::LocalScan(LocalScanTask {
                    request,
                    run,
                    token,
                    reply,
                }),
            ))
            .is_err()
        {
            *state = JobState::idle();
            return Err(anyhow!("the local scan service has stopped"));
        }
        Ok(done)
    }

    pub fn is_stopping(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }
}

pub(super) fn cancel_token(active: &ActiveCancel, id: &str) -> yas::cancel::CancelToken {
    let mut current = active.lock().unwrap();
    if let Some((key, token)) = current.as_ref() {
        if key == id {
            return token.clone();
        }
    }
    let token = yas::cancel::CancelToken::new();
    *current = Some((id.to_owned(), token.clone()));
    token
}

pub fn record_scan_count(state: &Mutex<JobState>, completed: usize, total: usize, key: &str) {
    if let Ok(mut state) = state.lock() {
        if let Some(scan) = &mut state.scan_progress {
            if let Some(phase) = phase_slot(scan, key) {
                phase.completed = completed;
                phase.total = total;
                phase.state = PhaseState::Running;
            }
        }
    }
}

pub fn record_scan_phase(
    state: &Mutex<JobState>,
    key: &str,
    event: crate::scanner::common::progress::ScanPhaseEvent,
) {
    use crate::scanner::common::progress::ScanPhaseEvent;
    if let Ok(mut state) = state.lock() {
        if let Some(scan) = &mut state.scan_progress {
            if let Some(phase) = phase_slot(scan, key) {
                phase.state = match event {
                    ScanPhaseEvent::Started => PhaseState::Running,
                    ScanPhaseEvent::Complete(count) => {
                        phase.completed = count;
                        PhaseState::Complete
                    },
                    ScanPhaseEvent::Interrupted => PhaseState::Aborted,
                };
            }
        }
    }
}

fn phase_slot<'a>(scan: &'a mut ScanProgress, key: &str) -> Option<&'a mut PhaseProgress> {
    match key {
        "characters" => scan.characters.as_mut(),
        "weapons" => scan.weapons.as_mut(),
        "artifacts" => scan.artifacts.as_mut(),
        "achievements" => scan.achievements.as_mut(),
        _ => None,
    }
}

pub(super) fn handle_stop(
    mut request: tiny_http::Request,
    state: &Mutex<JobState>,
    active: &ActiveCancel,
    origin: Option<&str>,
) {
    use std::io::Read;
    #[derive(serde::Deserialize)]
    struct StopRequest {
        #[serde(rename = "jobId")]
        job_id: String,
    }
    let mut body = String::new();
    let parsed = request
        .as_reader()
        .take(4097)
        .read_to_string(&mut body)
        .ok()
        .filter(|_| body.len() <= 4096)
        .and_then(|_| serde_json::from_str::<StopRequest>(&body).ok());
    let Some(parsed) = parsed.filter(|r| !r.job_id.is_empty()) else {
        respond_error(
            request,
            400,
            "请提供有效的 jobId。",
            "A valid jobId is required.",
            origin,
        );
        return;
    };
    let current = state.lock().unwrap();
    if current.job_id.as_deref() != Some(parsed.job_id.as_str()) {
        respond_error(
            request,
            404,
            "找不到该任务。",
            "The requested job was not found.",
            origin,
        );
        return;
    }
    let running = current.state == JobPhase::Running;
    if running && current.ui.kind.is_none() {
        respond_error(
            request,
            409,
            "当前点击已提交，请等待反馈。",
            "The click was submitted; wait for its feedback.",
            origin,
        );
        return;
    }
    if running {
        cancel_token(active, &parsed.job_id).cancel(yas::cancel::StopReason::UserAbort);
    }
    drop(current);
    respond_json(
        request,
        200,
        &serde_json::json!({"jobId":parsed.job_id,"stopRequested":running}).to_string(),
        origin,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn gui_scan_keeps_http_responsive_can_stop_and_can_start_again() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let slot: LocalScanSlot = Arc::new(Mutex::new(None));
        let shared = slot.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = shutdown.clone();
        let state = Arc::new(Mutex::new(JobState::idle()));
        let native = state.clone();
        let server = std::thread::spawn(move || {
            run_server_with_local_scans(
                port,
                || {
                    Err(anyhow!(
                        "a GUI scan must not initialize a second game executor"
                    ))
                },
                Arc::new(AtomicBool::new(true)),
                stop,
                false,
                None,
                Some(native),
                Some(shared),
            )
            .unwrap()
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let local = loop {
            if let Some(client) = slot.lock().unwrap().clone() {
                break client;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        };
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let base = format!("http://127.0.0.1:{port}");
        let request = || ScanRequest {
            characters: false,
            weapons: false,
            artifacts: true,
            achievements: false,
            artifact_mode: Default::default(),
            artifact_limit: None,
        };
        let token = yas::cancel::CancelToken::new();
        let inside = token.clone();
        let scan_state = state.clone();
        let done = local
            .submit(
                request(),
                token.clone(),
                Box::new(move || {
                    record_scan_count(&scan_state, 7, 100, "artifacts");
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while !inside.is_cancelled() {
                        assert!(Instant::now() < deadline);
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(anyhow!("stopped before export"))
                }),
            )
            .unwrap();
        let first: serde_json::Value = http
            .get(format!("{base}/status"))
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(first["state"], "running");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let progress: serde_json::Value = http
                .get(format!("{base}/status"))
                .send()
                .unwrap()
                .json()
                .unwrap();
            if progress["scanProgress"]["artifacts"]["completed"] == 7 {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(http
            .get(format!("{base}/health"))
            .send()
            .unwrap()
            .status()
            .is_success());
        assert_eq!(
            http.post(format!("{base}/scan"))
                .json(&request())
                .send()
                .unwrap()
                .status()
                .as_u16(),
            409
        );
        assert_eq!(
            http.post(format!("{base}/stop"))
                .json(&serde_json::json!({"jobId":"wrong-job"}))
                .send()
                .unwrap()
                .status()
                .as_u16(),
            404
        );
        assert!(!token.is_cancelled());
        let response = http
            .post(format!("{base}/stop"))
            .json(&serde_json::json!({"jobId":first["jobId"]}))
            .send()
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert!(done.recv_timeout(Duration::from_secs(3)).unwrap().is_err());
        assert_eq!(
            http.get(format!(
                "{base}/artifacts?jobId={}",
                first["jobId"].as_str().unwrap()
            ))
            .send()
            .unwrap()
            .status()
            .as_u16(),
            503
        );
        // A subsequent GUI scan uses a new token and the same live listener.
        let next_token = yas::cancel::CancelToken::new();
        let next = local
            .submit(
                request(),
                next_token.clone(),
                Box::new(|| Err(anyhow!("second scan probe"))),
            )
            .unwrap();
        assert!(!next_token.is_cancelled());
        assert!(next.recv_timeout(Duration::from_secs(3)).unwrap().is_err());
        shutdown.store(true, Ordering::Release);
        server.join().unwrap();
    }
}
