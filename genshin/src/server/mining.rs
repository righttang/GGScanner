use super::*;
use crate::mining::{MiningRequest, RouteStore};
use anyhow::Context;
use std::io::Read;

static LIBRARY: Mutex<()> = Mutex::new(());

fn enqueue(
    parsed: MiningRequest,
    state: &Mutex<JobState>,
    active: &local_scan::ActiveCancel,
    tx: &mpsc::Sender<(String, ExecutionRequest)>,
) -> std::result::Result<String, (u16, &'static str)> {
    let mut current = state.lock().unwrap();
    if current.state == JobPhase::Running {
        return Err((409, "Another task is running"));
    }
    let id = uuid::Uuid::new_v4().to_string();
    *current = JobState::running(id.clone(), 0);
    current.ui.kind = Some(JobKind::Mining);
    *active.lock().unwrap() = Some((id.clone(), yas::cancel::CancelToken::new()));
    if tx
        .send((id.clone(), ExecutionRequest::Mining(parsed)))
        .is_err()
    {
        *current = JobState::idle();
        *active.lock().unwrap() = None;
        return Err((503, "The execution service has stopped"));
    }
    Ok(id)
}

impl local_scan::LocalScanClient {
    pub fn submit_mining(&self, request: MiningRequest) -> Result<String> {
        anyhow::ensure!(!self.is_stopping(), "The execution service is stopping");
        request.routes()?;
        enqueue(request, &self.state, &self.active, &self.tx).map_err(|(_, error)| anyhow!(error))
    }
}

pub fn library(path: &str, input: Option<serde_json::Value>) -> Result<serde_json::Value> {
    let _lock = LIBRARY.lock().unwrap_or_else(|error| error.into_inner());
    let store = RouteStore::default();
    let entries = match (path, input) {
        ("/mining/routes", None) => store.list()?,
        ("/mining/routes/refresh", Some(_)) => store.refresh()?,
        ("/mining/routes/download", Some(input)) => {
            store.fetch(input["id"].as_str().context("A route ID is required")?)?
        },
        ("/mining/routes/import", Some(input)) => store.import(std::path::Path::new(
            input["path"].as_str().context("A local path is required")?,
        ))?,
        _ => anyhow::bail!("Unknown mining route operation"),
    };
    Ok(serde_json::to_value(entries)?)
}

fn body(request: &mut tiny_http::Request) -> Result<String> {
    let mut body = String::new();
    request.as_reader().take(65537).read_to_string(&mut body)?;
    anyhow::ensure!(body.len() <= 65536, "Mining request exceeds 64 KiB");
    Ok(body)
}

pub(super) fn handle_start(
    mut request: tiny_http::Request,
    state: &Mutex<JobState>,
    active: &local_scan::ActiveCancel,
    tx: &mpsc::Sender<(String, ExecutionRequest)>,
    enabled: &AtomicBool,
    origin: Option<&str>,
) {
    if !enabled.load(Ordering::Relaxed) {
        respond_error(
            request,
            403,
            "请先启用本机服务。",
            "Enable the local service first.",
            origin,
        );
        return;
    }
    let parsed = (|| -> Result<MiningRequest> {
        let parsed: MiningRequest = serde_json::from_str(&body(&mut request)?)?;
        parsed.routes()?;
        Ok(parsed)
    })();
    let parsed = match parsed {
        Ok(p) => p,
        Err(e) => {
            respond_json(
                request,
                400,
                &serde_json::json!({"error":format!("{e:#}")}).to_string(),
                origin,
            );
            return;
        },
    };
    let id = match enqueue(parsed, state, active, tx) {
        Ok(id) => id,
        Err((code, _)) => {
            if code == 409 {
                respond_error(
                    request,
                    code,
                    "已有任务正在运行。",
                    "Another task is running.",
                    origin,
                );
            } else {
                respond_error(
                    request,
                    code,
                    "本机执行服务已停止。",
                    "The execution service has stopped.",
                    origin,
                );
            }
            return;
        },
    };
    respond_json(
        request,
        202,
        &serde_json::json!({"jobId":id}).to_string(),
        origin,
    );
}

pub(super) fn handle_library(
    mut request: tiny_http::Request,
    method: Method,
    path: String,
    origin: Option<String>,
) {
    // Network/cache work runs separately so health and cancellation stay responsive.
    std::thread::spawn(move || {
        let result = (|| -> Result<serde_json::Value> {
            let input = match method {
                Method::Get => None,
                Method::Post => Some(serde_json::from_str(&body(&mut request)?)?),
                _ => anyhow::bail!("Unsupported mining library method"),
            };
            library(&path, input)
        })();
        match result {
            Ok(value) => respond_json(request, 200, &value.to_string(), origin.as_deref()),
            Err(error) => respond_json(
                request,
                400,
                &serde_json::json!({"error":format!("{error:#}")}).to_string(),
                origin.as_deref(),
            ),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_mining_shares_reservation_and_cancellation_with_http() {
        let (tx, rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(JobState::idle()));
        let active = Arc::new(Mutex::new(None));
        let client = local_scan::LocalScanClient {
            tx,
            state: state.clone(),
            active: active.clone(),
            shutdown: Arc::new(AtomicBool::new(false)),
        };
        let request = || MiningRequest {
            route_ids: vec![],
            stationary: true,
            ..Default::default()
        };
        let id = client.submit_mining(request()).unwrap();
        let (queued, job) = rx.recv().unwrap();
        assert_eq!(queued, id);
        assert!(matches!(job, ExecutionRequest::Mining(_)));
        assert!(client.submit_mining(request()).is_err());
        assert!(client.stop_job("previous-job").is_err());
        assert!(!active.lock().unwrap().as_ref().unwrap().1.is_cancelled());
        assert!(client.stop_job(&id).unwrap());
        assert!(active.lock().unwrap().as_ref().unwrap().1.is_cancelled());
        *state.lock().unwrap() = JobState::idle();
        assert_ne!(client.submit_mining(request()).unwrap(), id);
        assert!(!active.lock().unwrap().as_ref().unwrap().1.is_cancelled());
    }
    #[test]
    fn mining_admission_is_serialized_and_stop_targets_the_job() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = format!("http://{}", server.server_addr());
        let state = Arc::new(Mutex::new(JobState::idle()));
        let active = Arc::new(Mutex::new(None));
        let job = state.clone();
        let cancel = active.clone();
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            for request in server.incoming_requests().take(5) {
                if request.url() == "/stop" {
                    local_scan::handle_stop(request, &job, &cancel, None);
                } else {
                    handle_start(request, &job, &cancel, &tx, &AtomicBool::new(true), None);
                }
            }
        });
        let http = reqwest::blocking::Client::new();
        let invalid = http
            .post(format!("{address}/mining/start"))
            .json(&serde_json::json!({"routeIds":[]}))
            .send()
            .unwrap();
        assert_eq!(invalid.status(), 400);
        let first: serde_json::Value = http
            .post(format!("{address}/mining/start"))
            .json(&serde_json::json!({"routeIds":[],"stationary":true}))
            .send()
            .unwrap()
            .json()
            .unwrap();
        let id = first["jobId"].as_str().unwrap();
        let (queued, task) = rx.recv().unwrap();
        assert_eq!(queued, id);
        assert!(matches!(task, ExecutionRequest::Mining(_)));
        let competing = http
            .post(format!("{address}/mining/start"))
            .json(&serde_json::json!({"routeIds":[],"stationary":true}))
            .send()
            .unwrap();
        assert_eq!(competing.status(), 409);
        let old = http
            .post(format!("{address}/stop"))
            .json(&serde_json::json!({"jobId":"older-job"}))
            .send()
            .unwrap();
        assert_eq!(old.status(), 404);
        assert!(!active.lock().unwrap().as_ref().unwrap().1.is_cancelled());
        let stop = http
            .post(format!("{address}/stop"))
            .json(&serde_json::json!({"jobId":id}))
            .send()
            .unwrap();
        assert_eq!(stop.status(), 200);
        assert!(active.lock().unwrap().as_ref().unwrap().1.is_cancelled());
        thread.join().unwrap();
    }
}
