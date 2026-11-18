use crate::backend::Backend;
use congspeak_teamspeak::moderation::Ban;
use eframe::egui;
use tokio::sync::oneshot;

pub struct BanWindow {
    pub generation: u32,
    entries: Vec<Ban>,
    loading: Option<oneshot::Receiver<Result<Vec<Ban>, String>>>,
    error: Option<String>,
    pub removing: Option<(u32, u32)>,
}

impl BanWindow {
    pub fn new(backend: &Backend, ctx: &egui::Context, generation: u32) -> Self {
        let mut window = Self {
            generation,
            entries: Vec::new(),
            loading: None,
            error: None,
            removing: None,
        };
        window.refresh(backend, ctx);
        window
    }

    fn refresh(&mut self, backend: &Backend, ctx: &egui::Context) {
        let (tx, rx) = oneshot::channel();
        let session = backend.session.clone();
        let ctx = ctx.clone();
        let generation = self.generation;
        self.entries.clear();
        self.error = None;
        self.loading = Some(rx);
        backend.runtime().spawn(async move {
            let _ = tx.send(session.bans(generation).await);
            ctx.request_repaint();
        });
    }

    pub fn complete(&mut self, request_id: u32, success: bool) {
        if let Some((id, ban_id)) = self.removing
            && id == request_id
        {
            self.removing = None;
            if success {
                self.entries.retain(|ban| ban.id != ban_id);
            }
        }
    }

    pub fn show(
        &mut self,
        backend: &Backend,
        ctx: &egui::Context,
    ) -> (bool, Option<(u32, String)>) {
        if let Some(mut loading) = self.loading.take() {
            match loading.try_recv() {
                Ok(Ok(entries)) => self.entries = entries,
                Ok(Err(error)) => self.error = Some(error),
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.error = Some("封禁查询任务已退出".into());
                }
                Err(oneshot::error::TryRecvError::Empty) => self.loading = Some(loading),
            }
        }
        let busy = self.loading.is_some() || self.removing.is_some();
        let mut open = true;
        let mut selected = None;
        egui::Window::new("服务器封禁列表")
            .open(&mut open)
            .default_width(570.)
            .default_height(320.)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.add_enabled(!busy, egui::Button::new("刷新")).clicked() {
                        self.refresh(backend, ctx);
                    }
                    if busy {
                        ui.spinner();
                    } else if let Some(error) = &self.error {
                        ui.colored_label(egui::Color32::LIGHT_RED, error);
                    } else if self.entries.is_empty() {
                        ui.weak("当前没有封禁记录");
                    }
                });
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for ban in &self.entries {
                        ui.push_id(ban.id, |ui| {
                            let target = [&ban.name, &ban.uid, &ban.ip]
                                .into_iter()
                                .find(|value| !value.is_empty())
                                .map_or("未知目标", String::as_str);
                            ui.separator();
                            ui.horizontal_wrapped(|ui| {
                                ui.strong(format!("#{} {target}", ban.id));
                                if ui
                                    .add_enabled(!busy, egui::Button::new("解除封禁…"))
                                    .clicked()
                                {
                                    selected = Some((ban.id, target.to_owned()));
                                }
                            });
                            ui.horizontal_wrapped(|ui| {
                                if !ban.author.is_empty() {
                                    ui.label(format!("执行人 {}", ban.author));
                                }
                                if ban.seconds == 0 {
                                    ui.label("永久");
                                } else {
                                    ui.label(format!("封禁时长 {} 秒", ban.seconds));
                                }
                                ui.label(&ban.reason);
                            });
                            ui.horizontal_wrapped(|ui| {
                                if !ban.uid.is_empty() && ban.uid != target {
                                    ui.monospace(&ban.uid);
                                }
                                if !ban.ip.is_empty() && ban.ip != target {
                                    ui.monospace(&ban.ip);
                                }
                            });
                        });
                    }
                });
            });
        (open, selected)
    }
}
