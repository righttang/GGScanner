//! Local, serialized game-window clicks with delayed OCR feedback.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::time::Duration;
use yas::ocr::ImageToText;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OcrRegion {
    #[serde(default)]
    pub name: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClickRequest {
    #[serde(default)]
    pub request_id: Option<String>,
    pub x: u32,
    pub y: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<GameKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default = "default_delay")]
    pub delay_ms: u64,
    #[serde(default)]
    pub ocr: Vec<OcrRegion>,
}

/// Fixed game navigation actions; arbitrary system shortcuts are not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GameKey {
    Menu,
    PartySetup,
    Inventory,
    Character,
    Map,
    Interact,
    Confirm,
}

impl GameKey {
    fn input_key(self) -> enigo::Key {
        match self {
            Self::Menu => enigo::Key::Escape,
            Self::PartySetup => enigo::Key::Layout('l'),
            Self::Inventory => enigo::Key::Layout('b'),
            Self::Character => enigo::Key::Layout('c'),
            Self::Map => enigo::Key::Layout('m'),
            Self::Interact => enigo::Key::Layout('f'),
            Self::Confirm => enigo::Key::Return,
        }
    }
}

fn default_delay() -> u64 {
    500
}

impl ClickRequest {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.key.is_none() || self.text.is_none(),
            "Choose key or text, not both"
        );
        if let Some(text) = &self.text {
            anyhow::ensure!(
                !text.is_empty()
                    && text.chars().count() <= 64
                    && !text.chars().any(char::is_control),
                "Text must contain 1 to 64 printable characters"
            );
        }
        anyhow::ensure!(self.delay_ms <= 5_000, "delayMs must be between 0 and 5000");
        anyhow::ensure!(self.ocr.len() <= 16, "At most 16 OCR regions are supported");
        if let Some(id) = &self.request_id {
            anyhow::ensure!(
                !id.is_empty() && id.len() <= 128,
                "requestId must contain 1 to 128 bytes"
            );
            anyhow::ensure!(
                id.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                "requestId must use letters, digits, hyphens or underscores"
            );
        }
        for region in &self.ocr {
            anyhow::ensure!(
                region.width > 0 && region.height > 0,
                "OCR regions must have positive dimensions"
            );
            anyhow::ensure!(
                region.name.len() <= 128,
                "OCR region names must not exceed 128 bytes"
            );
        }
        Ok(())
    }

    pub fn validate_window(&self, width: u32, height: u32) -> Result<()> {
        self.validate()?;
        anyhow::ensure!(
            self.x < width && self.y < height,
            "Click is outside the game client area ({width}x{height})"
        );
        for region in &self.ocr {
            anyhow::ensure!(
                region
                    .x
                    .checked_add(region.width)
                    .is_some_and(|x| x <= width)
                    && region
                        .y
                        .checked_add(region.height)
                        .is_some_and(|y| y <= height),
                "OCR region '{}' is outside the game client area",
                region.name
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrReading {
    #[serde(flatten)]
    pub region: OcrRegion,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClickFeedback {
    /// null means input may have executed; callers must observe before retrying.
    pub clicked: Option<bool>,
    pub width: u32,
    pub height: u32,
    pub ocr: Vec<OcrReading>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ClickFeedback {
    pub fn failed(error: impl ToString) -> Self {
        Self {
            clicked: Some(false),
            width: 0,
            height: 0,
            ocr: vec![],
            error: Some(error.to_string()),
        }
    }
}

pub(super) struct InputTask {
    pub request: ClickRequest,
    pub reply: mpsc::SyncSender<ClickFeedback>,
}

#[derive(Clone)]
struct Receipt {
    request: ClickRequest,
    feedback: Option<ClickFeedback>,
}

#[derive(Default)]
pub(super) struct InputReceipts {
    receipts: HashMap<String, Receipt>,
    order: VecDeque<String>,
}

impl InputReceipts {
    fn insert(&mut self, id: String, request: ClickRequest) {
        while self.order.len() >= 64 {
            if let Some(old) = self.order.pop_front() {
                self.receipts.remove(&old);
            }
        }
        self.order.push_back(id.clone());
        self.receipts.insert(
            id,
            Receipt {
                request,
                feedback: None,
            },
        );
    }

    pub fn finish(&mut self, id: &str, feedback: ClickFeedback) {
        if let Some(receipt) = self.receipts.get_mut(id) {
            receipt.feedback = Some(feedback);
        }
    }
}

fn respond_receipt(
    request: tiny_http::Request,
    id: &str,
    feedback: Option<&ClickFeedback>,
    origin: Option<&str>,
) {
    let (status, body) = if let Some(feedback) = feedback {
        let status = 200;
        (
            status,
            serde_json::json!({"jobId":id,"state":"completed","feedback":feedback}),
        )
    } else {
        (202, serde_json::json!({"jobId":id,"state":"running"}))
    };
    respond_json(request, status, &body.to_string(), origin);
}

pub(super) fn local_origin_only(origin: Option<&str>) -> bool {
    origin.is_none_or(|origin| {
        parse_http_origin(origin.trim_end_matches('/'))
            .is_some_and(|(_, host)| is_loopback_host(host))
    })
}

pub(super) fn handle_click(
    mut request: tiny_http::Request,
    enabled: &AtomicBool,
    state: &Arc<Mutex<JobState>>,
    job_tx: &mpsc::Sender<(String, ExecutionRequest)>,
    receipts: &Arc<Mutex<InputReceipts>>,
    origin: Option<&str>,
) {
    if !enabled.load(Ordering::Relaxed) {
        respond_error(
            request,
            503,
            "管理器已暂停。",
            "The manager is paused.",
            origin,
        );
        return;
    }
    if request.body_length().is_some_and(|size| size > 16_384) {
        respond_error(
            request,
            413,
            "点击请求过大。",
            "Click request is too large.",
            origin,
        );
        return;
    }
    let mut body = String::new();
    if std::io::Read::take(request.as_reader(), 16_385)
        .read_to_string(&mut body)
        .is_err()
        || body.len() > 16_384
    {
        respond_error(
            request,
            400,
            "无法读取点击请求。",
            "Could not read click request.",
            origin,
        );
        return;
    }
    let click: ClickRequest = match serde_json::from_str(&body) {
        Ok(click) => click,
        Err(error) => {
            respond_error_with_diagnostic(
                request,
                400,
                "点击请求无效。",
                "Invalid click request.",
                error,
                origin,
            );
            return;
        },
    };
    if let Err(error) = click.validate() {
        respond_error_with_diagnostic(
            request,
            400,
            "点击请求无效。",
            "Invalid click request.",
            error,
            origin,
        );
        return;
    }
    let id = click
        .request_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if let Some(receipt) = receipts.lock().unwrap().receipts.get(&id).cloned() {
        if receipt.request != click {
            respond_error(
                request,
                409,
                "requestId 已用于不同的点击请求。",
                "requestId was already used for a different click request.",
                origin,
            );
        } else {
            respond_receipt(request, &id, receipt.feedback.as_ref(), origin);
        }
        return;
    }
    {
        let mut state = state.lock().unwrap();
        if state.state == JobPhase::Running {
            respond_error(
                request,
                409,
                "另一个任务正在运行，拒绝点击。",
                "Another job is running; click rejected.",
                origin,
            );
            return;
        }
        *state = JobState::running(id.clone(), 1);
    }
    receipts.lock().unwrap().insert(id.clone(), click.clone());
    save_request("input_click", &body);
    let (reply, rx) = mpsc::sync_channel(1);
    if job_tx
        .send((
            id.clone(),
            ExecutionRequest::Input(InputTask {
                request: click,
                reply,
            }),
        ))
        .is_err()
    {
        let feedback = ClickFeedback::failed("Game execution thread is unavailable");
        receipts.lock().unwrap().finish(&id, feedback.clone());
        *state.lock().unwrap() = JobState::idle();
        respond_receipt(request, &id, Some(&feedback), origin);
        return;
    }
    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(feedback) => respond_receipt(request, &id, Some(&feedback), origin),
        // Execution remains observable and is never resubmitted by this handler.
        Err(_) => respond_receipt(request, &id, None, origin),
    }
}

pub(super) fn handle_result(
    request: tiny_http::Request,
    url: &str,
    receipts: &Arc<Mutex<InputReceipts>>,
    origin: Option<&str>,
) {
    let id = url.split_once('?').and_then(|(_, query)| {
        query
            .split('&')
            .find_map(|part| part.strip_prefix("jobId="))
    });
    let Some(id) = id else {
        respond_error(request, 400, "需要 jobId。", "jobId is required.", origin);
        return;
    };
    let receipt = receipts.lock().unwrap().receipts.get(id).cloned();
    if let Some(receipt) = receipt {
        respond_receipt(request, id, receipt.feedback.as_ref(), origin);
    } else {
        respond_error(
            request,
            404,
            "未找到点击结果。",
            "Click result was not found.",
            origin,
        );
    }
}

pub(super) fn execute_game_click(exec: &mut GameExecutor, request: &ClickRequest) -> ClickFeedback {
    let mut feedback = ClickFeedback::failed("Click was not executed");
    let execute = (|| -> Result<()> {
        exec.ctrl.focus_game_window_quick();
        let rect = exec.ctrl.game_info.window;
        request.validate_window(rect.width.max(0) as u32, rect.height.max(0) as u32)?;
        feedback.width = rect.width as u32;
        feedback.height = rect.height as u32;
        anyhow::ensure!(
            exec.ctrl.game_info.native_window().is_foreground(),
            "Game window could not be focused"
        );
        let token = yas::cancel::CancelToken::new();
        exec.ctrl.set_cancel_token(token);
        anyhow::ensure!(
            !exec.ctrl.check_rmb(),
            "Click cancelled by right mouse button"
        );
        // A backend error can occur after partial input; never label it unclicked.
        feedback.clicked = None;
        if let Some(key) = request.key {
            exec.ctrl.system_control.key_press(key.input_key())?;
        } else {
            exec.ctrl.click_client_at(request.x, request.y)?;
            if let Some(text) = &request.text {
                std::thread::sleep(Duration::from_millis(100));
                anyhow::ensure!(
                    exec.ctrl.game_info.native_window().is_foreground(),
                    "Game focus changed before text input"
                );
                #[cfg(target_os = "windows")]
                exec.ctrl.system_control.replace_text(text)?;
                #[cfg(not(target_os = "windows"))]
                anyhow::bail!("Game text input is currently supported on Windows only");
            }
        }
        feedback.clicked = Some(true);
        std::thread::sleep(Duration::from_millis(request.delay_ms));
        if !request.ocr.is_empty() {
            let image = exec.ctrl.capture_game()?;
            feedback.width = image.width();
            feedback.height = image.height();
            let model = exec.manager.pools().artifact().v4().get();
            for region in &request.ocr {
                let text = (|| -> Result<String> {
                    anyhow::ensure!(
                        region
                            .x
                            .checked_add(region.width)
                            .is_some_and(|x| x <= image.width())
                            && region
                                .y
                                .checked_add(region.height)
                                .is_some_and(|y| y <= image.height()),
                        "Game window resized before OCR"
                    );
                    let crop = image::imageops::crop_imm(
                        &image,
                        region.x,
                        region.y,
                        region.width,
                        region.height,
                    )
                    .to_image();
                    model.image_to_text(&crop, false)
                })();
                feedback.ocr.push(OcrReading {
                    region: region.clone(),
                    text: text.as_ref().cloned().unwrap_or_default(),
                    error: text.err().map(|e| e.to_string()),
                });
            }
        }
        Ok(())
    })();
    feedback.error = execute.err().map(|error| format!("{error:#}"));
    feedback
}

pub(super) fn execute_task<F>(
    task: &InputTask,
    executor: &mut Option<Box<dyn ManageExecutor>>,
    init: &mut F,
) -> ClickFeedback
where
    F: FnMut() -> Result<Box<dyn ManageExecutor>>,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if executor.is_none() {
            *executor = Some(init()?);
        }
        let executor = executor.as_mut().unwrap();
        executor.prepare_task()?;
        Ok::<_, anyhow::Error>(executor.execute_click(&task.request))
    })) {
        Ok(Ok(feedback)) => feedback,
        Ok(Err(error)) => ClickFeedback::failed(format!("{error:#}")),
        Err(_) => {
            *executor = None;
            ClickFeedback {
                clicked: None,
                width: 0,
                height: 0,
                ocr: vec![],
                error: Some("Input execution panicked; observe the game before any retry".into()),
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    struct FakeInput {
        clicks: Arc<Mutex<Vec<ClickRequest>>>,
    }

    impl ManageExecutor for FakeInput {
        fn execute_click(&mut self, request: &ClickRequest) -> ClickFeedback {
            if let Err(error) = request.validate_window(1920, 1080) {
                return ClickFeedback::failed(error);
            }
            self.clicks.lock().unwrap().push(request.clone());
            ClickFeedback {
                clicked: Some(true),
                width: 1920,
                height: 1080,
                ocr: request
                    .ocr
                    .iter()
                    .map(|region| OcrReading {
                        region: region.clone(),
                        text: "+4".into(),
                        error: None,
                    })
                    .collect(),
                error: None,
            }
        }

        fn execute(
            &mut self,
            _: LockManageRequest,
            _: Option<&ProgressFn<'_>>,
            _: yas::cancel::CancelToken,
        ) -> (ManageResult, Option<Vec<GoodArtifact>>) {
            panic!("unexpected manage call")
        }

        fn execute_equip(
            &mut self,
            _: EquipRequest,
            _: Option<&ProgressFn<'_>>,
            _: yas::cancel::CancelToken,
        ) -> ManageResult {
            panic!("unexpected equip call")
        }

        fn execute_scan(
            &mut self,
            _: &ScanRequest,
            _: Option<&crate::scanner::common::progress::ProgressFn<'_>>,
            _: yas::cancel::CancelToken,
        ) -> Result<ScanResult> {
            panic!("unexpected scan call")
        }
    }

    struct TestServer {
        port: u16,
        shutdown: Arc<AtomicBool>,
        enabled: Arc<AtomicBool>,
        state: Arc<Mutex<JobState>>,
        clicks: Arc<Mutex<Vec<ClickRequest>>>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl TestServer {
        fn new() -> Self {
            let socket = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = socket.local_addr().unwrap().port();
            drop(socket);
            let shutdown = Arc::new(AtomicBool::new(false));
            let enabled = Arc::new(AtomicBool::new(true));
            let state = Arc::new(Mutex::new(JobState::idle()));
            let clicks = Arc::new(Mutex::new(vec![]));
            let (stop, enable, job, calls) = (
                shutdown.clone(),
                enabled.clone(),
                state.clone(),
                clicks.clone(),
            );
            let handle = std::thread::spawn(move || {
                run_server_observed(
                    port,
                    move || {
                        Ok(Box::new(FakeInput {
                            clicks: calls.clone(),
                        }) as Box<dyn ManageExecutor>)
                    },
                    enable,
                    stop,
                    false,
                    None,
                    Some(job),
                )
                .unwrap();
            });
            let server = Self {
                port,
                shutdown,
                enabled,
                state,
                clicks,
                handle: Some(handle),
            };
            for _ in 0..100 {
                if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                    return server;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            panic!("input test server did not start");
        }

        fn request(
            &self,
            method: &str,
            path: &str,
            body: &str,
            origin: Option<&str>,
        ) -> (u16, serde_json::Value) {
            let mut socket = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(20)))
                .unwrap();
            let origin = origin
                .map(|o| format!("Origin: {o}\r\n"))
                .unwrap_or_default();
            write!(socket,"{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n{origin}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).unwrap();
            let mut result = String::new();
            socket.read_to_string(&mut result).unwrap();
            let status = result.split_whitespace().nth(1).unwrap().parse().unwrap();
            let body = result.split_once("\r\n\r\n").unwrap().1;
            (
                status,
                serde_json::from_str(body).unwrap_or(serde_json::Value::Null),
            )
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Relaxed);
            if let Some(handle) = self.handle.take() {
                handle.join().unwrap();
            }
        }
    }

    #[test]
    fn navigation_and_text_requests_are_bounded_and_exclusive() {
        let parse = |value| serde_json::from_value::<ClickRequest>(value);
        assert!(parse(serde_json::json!({"x":0,"y":0,"key":"partySetup"}))
            .unwrap()
            .validate()
            .is_ok());
        assert!(parse(serde_json::json!({"x":0,"y":0,"key":"winR"})).is_err());
        for text in [
            String::new(),
            "队".repeat(65),
            "队伍\n第二行".into(),
            "队伍\u{0}".into(),
        ] {
            assert!(parse(serde_json::json!({"x":0,"y":0,"text":text}))
                .unwrap()
                .validate()
                .is_err());
        }
        assert!(
            parse(serde_json::json!({"x":0,"y":0,"text":"01 薇斯纳·星扩散"}))
                .unwrap()
                .validate()
                .is_ok()
        );
        assert!(
            parse(serde_json::json!({"x":0,"y":0,"key":"confirm","text":"队伍"}))
                .unwrap()
                .validate()
                .is_err()
        );
    }

    #[test]
    fn key_requests_share_receipts_and_reject_changed_retries() {
        let server = TestServer::new();
        let body = r#"{"requestId":"party-once","x":0,"y":0,"key":"partySetup","delayMs":0}"#;
        assert_eq!(server.request("POST", "/input/click", body, None).0, 200);
        assert_eq!(server.request("POST", "/input/click", body, None).0, 200);
        assert_eq!(server.clicks.lock().unwrap().len(), 1);
        assert_eq!(
            server.clicks.lock().unwrap()[0].key,
            Some(GameKey::PartySetup)
        );
        let changed = body.replace("partySetup", "menu");
        assert_eq!(
            server.request("POST", "/input/click", &changed, None).0,
            409
        );
        assert_eq!(server.clicks.lock().unwrap().len(), 1);
    }

    #[test]
    fn click_validation_rejects_overflow_bounds_and_unbounded_waits() {
        let parse = |body: &str| serde_json::from_str::<ClickRequest>(body);
        assert!(parse(r#"{"x":-1,"y":0}"#).is_err());
        assert!(parse(r#"{"x":0,"y":0,"unexpected":true}"#).is_err());
        assert!(parse(r#"{"x":0,"y":0,"delayMs":5001}"#)
            .unwrap()
            .validate()
            .is_err());
        assert!(parse(r#"{"x":0,"y":0,"requestId":"x&jobId=y"}"#)
            .unwrap()
            .validate()
            .is_err());
        assert!(parse(r#"{"x":1919,"y":1079}"#)
            .unwrap()
            .validate_window(1920, 1080)
            .is_ok());
        assert!(parse(r#"{"x":1920,"y":0}"#)
            .unwrap()
            .validate_window(1920, 1080)
            .is_err());
        assert!(
            parse(r#"{"x":0,"y":0,"ocr":[{"x":4294967295,"y":0,"width":1,"height":20}]}"#)
                .unwrap()
                .validate_window(1920, 1080)
                .is_err()
        );
    }

    #[test]
    fn click_http_feedback_is_idempotent_and_pollable() {
        let server = TestServer::new();
        let body = r#"{"requestId":"once","x":12,"y":34,"delayMs":321,"ocr":[{"name":"level","x":100,"y":100,"width":80,"height":35}]}"#;
        let (status, feedback) = server.request("POST", "/input/click", body, None);
        assert_eq!(status, 200);
        assert_eq!(feedback["feedback"]["clicked"], true);
        assert_eq!(feedback["feedback"]["ocr"][0]["text"], "+4");
        assert_eq!(feedback["feedback"]["ocr"][0]["name"], "level");
        assert_eq!(server.clicks.lock().unwrap()[0].delay_ms, 321);
        assert_eq!(
            server.request("POST", "/input/click", body, None).1,
            feedback
        );
        assert_eq!(server.clicks.lock().unwrap().len(), 1);
        assert_eq!(
            server
                .request("GET", "/input/result?jobId=once", "", None)
                .1,
            feedback
        );
        assert_eq!(
            server
                .request(
                    "POST",
                    "/input/click",
                    r#"{"requestId":"once","x":99,"y":34}"#,
                    None
                )
                .0,
            409
        );
        assert_eq!(
            server
                .request("POST", "/input/click", r#"{"x":1920,"y":0}"#, None)
                .0,
            200
        );
        assert_eq!(server.clicks.lock().unwrap().len(), 1);
    }

    #[test]
    fn click_http_refuses_remote_origins_disabled_and_busy_manager() {
        let server = TestServer::new();
        let body = r#"{"x":100,"y":100}"#;
        assert_eq!(
            server
                .request("POST", "/input/click", body, Some("https://ggartifact.com"))
                .0,
            403
        );
        assert_eq!(
            server
                .request(
                    "OPTIONS",
                    "/input/click",
                    "",
                    Some("https://ggartifact.com")
                )
                .0,
            403
        );
        server.enabled.store(false, Ordering::Relaxed);
        assert_eq!(server.request("POST", "/input/click", body, None).0, 503);
        server.enabled.store(true, Ordering::Relaxed);
        *server.state.lock().unwrap() = JobState::running("scan-in-progress".into(), 10);
        assert_eq!(server.request("POST", "/input/click", body, None).0, 409);
        assert!(server.clicks.lock().unwrap().is_empty());
    }
}
