use super::state::Lang;
use eframe::egui;
use genshin_scanner::{
    manager::models::{JobPhase, JobState},
    mining::RouteEntry,
};
use std::{
    collections::BTreeSet,
    sync::{mpsc, Arc, Mutex},
    time::Duration,
};

pub struct MiningUi {
    local: genshin_scanner::server::LocalScanSlot,
    service_status: Arc<Mutex<super::state::TaskStatus>>,
    entries: Vec<RouteEntry>,
    selected: BTreeSet<String>,
    pending: Option<mpsc::Receiver<Result<serde_json::Value, String>>>,
    cancel_pending: Option<mpsc::Receiver<Result<serde_json::Value, String>>>,
    error: Option<String>,
}

impl Default for MiningUi {
    fn default() -> Self {
        let (entries, error) = match genshin_scanner::mining::RouteStore::default().list() {
            Ok(entries) => (entries, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        Self {
            local: Arc::new(Mutex::new(None)),
            service_status: Arc::new(Mutex::new(super::state::TaskStatus::Idle)),
            entries,
            selected: Default::default(),
            pending: None,
            cancel_pending: None,
            error,
        }
    }
}

impl MiningUi {
    pub(super) fn new(
        local: genshin_scanner::server::LocalScanSlot,
        service_status: Arc<Mutex<super::state::TaskStatus>>,
    ) -> Self {
        Self {
            local,
            service_status,
            ..Self::default()
        }
    }
    pub(super) fn is_loading(&self) -> bool {
        self.pending.is_some() || self.cancel_pending.is_some()
    }
    fn request(&mut self, ui: &egui::Ui, path: &str, body: Option<serde_json::Value>) {
        let pending = if path == "/stop" {
            &mut self.cancel_pending
        } else {
            &mut self.pending
        };
        if pending.is_some() {
            return;
        }
        self.error = None;
        let ctx = ui.ctx().clone();
        let path = path.to_owned();
        let local = self.local.clone();
        let (tx, rx) = mpsc::channel();
        *pending = Some(rx);
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<serde_json::Value> {
                match path.as_str() {
                    "/mining/start" | "/stop" => {
                        let client = local.lock().unwrap().clone().ok_or_else(|| {
                            anyhow::anyhow!("The local execution service is not ready")
                        })?;
                        let input = body.unwrap_or_default();
                        if path == "/mining/start" {
                            let id = client.submit_mining(serde_json::from_value(input)?)?;
                            Ok(serde_json::json!({"jobId":id}))
                        } else {
                            let id = input["jobId"]
                                .as_str()
                                .ok_or_else(|| anyhow::anyhow!("A job ID is required"))?;
                            Ok(serde_json::json!({"jobId":id,"stopRequested":client.stop_job(id)?}))
                        }
                    },
                    _ => genshin_scanner::server::mining_library(&path, body),
                }
            })()
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }
    fn poll(&mut self) {
        if let Some(rx) = &self.cancel_pending {
            match rx.try_recv() {
                Ok(Ok(_)) => self.cancel_pending = None,
                Ok(Err(error)) => {
                    self.error = Some(error);
                    self.cancel_pending = None;
                },
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.error = Some("Stop request did not finish".into());
                    self.cancel_pending = None;
                },
                Err(mpsc::TryRecvError::Empty) => {},
            }
        }
        let received = self.pending.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(value) => Some(value),
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("Local worker stopped".into())),
            Err(_) => None,
        });
        if let Some(result) = received {
            self.pending = None;
            match result {
                Ok(value) if value.is_array() => match serde_json::from_value(value) {
                    Ok(entries) => {
                        self.entries = entries;
                        self.selected.retain(|id| {
                            self.entries.iter().any(|entry| {
                                &entry.id == id && entry.downloaded && entry.issues.is_empty()
                            })
                        });
                    },
                    Err(e) => self.error = Some(e.to_string()),
                },
                Ok(_) => {},
                Err(e) => self.error = Some(e),
            }
        }
    }
    pub fn settings(&mut self, ui: &mut egui::Ui, l: Lang, blocked: bool) {
        self.poll();
        let ready = self
            .local
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|client| !client.is_stopping());
        let blocked = blocked || !ready;
        if !ready {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.pending.is_none(),
                    egui::Button::new(l.t("刷新社区路线", "Refresh community routes")),
                )
                .clicked()
            {
                self.request(ui, "/mining/routes/refresh", Some(serde_json::json!({})));
            }
            if ui
                .add_enabled(
                    self.pending.is_none(),
                    egui::Button::new(l.t("导入 JSON", "Import JSON")),
                )
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("JSON", &["json"])
                    .pick_file()
                {
                    self.request(
                        ui,
                        "/mining/routes/import",
                        Some(serde_json::json!({"path":path})),
                    );
                }
            }
            if ui
                .add_enabled(
                    self.pending.is_none(),
                    egui::Button::new(l.t("导入文件夹", "Import folder")),
                )
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.request(
                        ui,
                        "/mining/routes/import",
                        Some(serde_json::json!({"path":path})),
                    );
                }
            }
        });
        ui.separator();
        let mut download = None;
        egui::ScrollArea::vertical()
            .id_salt("mining-routes")
            .max_height((ui.available_height() - 150.0).clamp(80.0, 360.0))
            .show(ui, |ui| {
                for entry in &self.entries {
                    ui.horizontal_wrapped(|ui| {
                        let mut selected = self.selected.contains(&entry.id);
                        if ui
                            .add_enabled(
                                entry.downloaded && entry.issues.is_empty() && !blocked,
                                egui::Checkbox::new(&mut selected, &entry.name),
                            )
                            .changed()
                        {
                            if selected {
                                self.selected.insert(entry.id.clone());
                            } else {
                                self.selected.remove(&entry.id);
                            }
                        }
                        if !entry.downloaded
                            && ui
                                .add_enabled(
                                    self.pending.is_none(),
                                    egui::Button::new(l.t("下载并检查", "Download and check")),
                                )
                                .clicked()
                        {
                            download = Some(entry.id.clone());
                        }
                    });
                    if !entry.authors.is_empty() {
                        ui.small(entry.authors.join(", "));
                    }
                    for issue in &entry.issues {
                        let issue = match issue.as_str() {
                            "Teyvat surface routes only" => {
                                l.t("仅支持提瓦特地表", "Teyvat surface routes only")
                            },
                            "First waypoint must be a teleport" => {
                                l.t("首点须为传送点", "First waypoint must be a teleport")
                            },
                            other => other,
                        };
                        ui.colored_label(ui.visuals().warn_fg_color, issue);
                    }
                }
            });
        if let Some(id) = download {
            self.request(
                ui,
                "/mining/routes/download",
                Some(serde_json::json!({"id":id})),
            );
        }
        ui.separator();
        ui.label(l.t("当前出战队伍须包含莉奈娅。按列表顺序运行，每条一次。鼠标右键或停止按钮可终止。","The active party needs Linnea. Run once in list order. Right-click or press Stop to cancel."));
        if ui
            .add_enabled(
                !blocked && self.pending.is_none() && !self.selected.is_empty(),
                egui::Button::new(l.t("开始挖矿", "Start mining")),
            )
            .clicked()
        {
            let ids: Vec<_> = self
                .entries
                .iter()
                .filter(|e| self.selected.contains(&e.id))
                .map(|e| e.id.clone())
                .collect();
            self.request(
                ui,
                "/mining/start",
                Some(serde_json::json!({"routeIds":ids})),
            );
        }
        if ui
            .add_enabled(
                !blocked && self.pending.is_none(),
                egui::Button::new(l.t("当前位置试采一次", "Test mining here")),
            )
            .clicked()
        {
            self.request(
                ui,
                "/mining/start",
                Some(serde_json::json!({"routeIds":[],"stationary":true})),
            );
        }
    }
    pub fn status(&mut self, ui: &mut egui::Ui, l: Lang, state: &Arc<Mutex<JobState>>) {
        self.poll();
        if self.is_loading() {
            ui.spinner();
            ui.label(l.t("正在处理…", "Working…"));
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        if let Some(error) = &self.error {
            ui.colored_label(
                ui.visuals().error_fg_color,
                l.t(
                    "操作未完成，请查看错误详情。",
                    "The operation could not finish. See error details.",
                ),
            );
            ui.label(error);
        }
        let state = state.lock().unwrap().clone();
        if self
            .local
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(|client| client.is_stopping())
        {
            let status = super::worker::try_task_status(&self.service_status);
            super::theme::task_status(
                ui,
                l,
                status.as_ref(),
                l.t(
                    "正在准备本机执行服务",
                    "Preparing the local execution service",
                ),
            );
        }
        let Some(report) = state.mining else {
            ui.label(l.t(
                "选择路线，或在当前矿点试采。",
                "Select routes or test at the current ore deposit.",
            ));
            return;
        };
        let phase = match report.phase.as_str() {
            "preparing" => l.t("准备", "Preparing"),
            "loadingModels" => l.t("加载模型与地图", "Loading models and map"),
            "teleporting" => l.t("传送", "Teleporting"),
            "moving" => l.t("跑线", "Moving"),
            "searchingOre" => l.t("寻找矿石", "Searching for ore"),
            "aiming" => l.t("瞄准", "Aiming"),
            "shooting" => l.t("射击并确认收获", "Shooting and checking loot"),
            "completed" => l.t("已完成", "Completed"),
            "stopped" => l.t("已停止", "Stopped"),
            _ => l.t("未能完成", "Failed"),
        };
        ui.heading(phase);
        ui.label(route_label(&report.route, l));
        ui.label(format!(
            "{} / {}",
            report.completed_points, report.total_points
        ));
        if report.total_points > 0 {
            ui.add(egui::ProgressBar::new(
                report.completed_points as f32 / report.total_points as f32,
            ));
        }
        if state.state == JobPhase::Running
            && ui
                .add_enabled(
                    self.cancel_pending.is_none(),
                    egui::Button::new(l.t("停止挖矿", "Stop mining")),
                )
                .clicked()
        {
            self.request(ui, "/stop", Some(serde_json::json!({"jobId":state.job_id})));
        }
        for (name, count) in &report.loot {
            ui.label(format!("{name} × {count}"));
        }
        for point in &report.points {
            let outcome = match point.outcome.as_str() {
                "aimFailed" => l.t("识别到矿石，瞄准未完成", "Ore detected, aiming incomplete"),
                "collected" => l.t("已确认获得", "Collection confirmed"),
                "noOre" => l.t("未发现矿石", "No ore detected"),
                "interrupted" => l.t("已中断", "Interrupted"),
                _ => l.t("已射击，收获待确认", "Shot fired, loot unconfirmed"),
            };
            ui.label(format!(
                "{} · {} · {}",
                route_label(&point.route, l),
                point.waypoint,
                outcome
            ));
        }
        if let Some(error) = report.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if state.state == JobPhase::Running {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }
}

pub(super) fn route_label(route: &str, lang: Lang) -> &str {
    if route == "stationary" {
        lang.t("单点测试", "Stationary test")
    } else {
        route
    }
}
