use crate::backend::Backend;
use congspeak_core::Channel;
use congspeak_teamspeak::files::{Entry, Location, Mutation, Progress, child_path};
use eframe::egui;
use std::{future::Future, path::PathBuf, time::Duration};
use tokio::{
    sync::{oneshot, watch},
    task::AbortHandle,
};

enum Operation {
    Folder,
    Rename(String),
    Delete(String),
    Upload,
    Download(String),
}

struct Form {
    operation: Operation,
    value: String,
}

struct Running {
    result: oneshot::Receiver<Result<Option<Vec<Entry>>, String>>,
    progress: Option<watch::Receiver<Progress>>,
    abort: AbortHandle,
    success: &'static str,
    refresh: bool,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

pub struct Browser {
    channel_name: String,
    has_password: bool,
    location: Location,
    password: String,
    entries: Vec<Entry>,
    selection: Option<usize>,
    form: Option<Form>,
    running: Option<Running>,
    status: String,
    initial_load: bool,
}

impl Browser {
    pub fn new(channel: &Channel, generation: u32) -> Self {
        Self {
            channel_name: channel.name.clone(),
            has_password: channel.has_password,
            location: Location {
                generation,
                channel: channel.id.clone(),
                path: "/".into(),
                password: None,
            },
            password: String::new(),
            entries: vec![],
            selection: None,
            form: None,
            running: None,
            status: if channel.has_password {
                "输入频道密码后点击刷新".into()
            } else {
                String::new()
            },
            initial_load: !channel.has_password,
        }
    }

    fn location(&self, path: String) -> Location {
        Location {
            generation: self.location.generation,
            channel: self.location.channel.clone(),
            path,
            password: Some(self.password.clone()).filter(|p| !p.is_empty()),
        }
    }

    fn start(
        &mut self,
        backend: &Backend,
        ctx: &egui::Context,
        work: impl Future<Output = Result<Option<Vec<Entry>>, String>> + Send + 'static,
        progress: Option<watch::Receiver<Progress>>,
        success: &'static str,
        refresh: bool,
    ) {
        let (send, result) = oneshot::channel();
        let ctx = ctx.clone();
        let abort = backend
            .runtime()
            .spawn(async move {
                let result = work.await;
                let _ = send.send(result);
                ctx.request_repaint();
            })
            .abort_handle();
        self.running = Some(Running {
            result,
            progress,
            abort,
            success,
            refresh,
        });
        self.status = "等待服务器响应…".into();
    }

    fn list(&mut self, backend: &Backend, ctx: &egui::Context, path: String) {
        let session = backend.session.clone();
        let location = self.location(path.clone());
        self.location.path = path;
        self.entries.clear();
        self.selection = None;
        self.start(
            backend,
            ctx,
            async move { session.file_list(location).await.map(Some) },
            None,
            "目录已更新",
            false,
        );
    }

    fn results(&mut self, ctx: &egui::Context, backend: &Backend) {
        let Some(running) = &mut self.running else {
            return;
        };
        let result = match running.result.try_recv() {
            Ok(result) => result,
            Err(oneshot::error::TryRecvError::Empty) => return,
            Err(oneshot::error::TryRecvError::Closed) => Err("文件任务已停止".into()),
        };
        let running = self.running.take().unwrap();
        match result {
            Ok(entries) => {
                self.status = running.success.into();
                if let Some(entries) = entries {
                    self.entries = entries;
                    self.selection = None;
                }
                if running.refresh {
                    let status = self.status.clone();
                    self.list(backend, ctx, self.location.path.clone());
                    self.status = status;
                    if let Some(job) = &mut self.running {
                        job.success = running.success;
                    }
                }
            }
            Err(error) => self.status = error,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, backend: &Backend) -> bool {
        self.results(ctx, backend);
        if std::mem::take(&mut self.initial_load) {
            self.list(backend, ctx, "/".into());
        }
        let mut open = true;
        let mut navigate = None;
        let mut dropped = None;
        let mut window = egui::Window::new(format!("频道文件 · {}", self.channel_name))
            .id(egui::Id::unique("channel-files"))
            .default_width(600.)
            .default_height(380.);
        if self.running.is_none() {
            window = window.open(&mut open);
        }
        window.show(ctx, |ui| {
            let busy = self.running.is_some();
            ui.add_enabled_ui(!busy && self.form.is_none(), |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(self.location.path != "/", egui::Button::new("上一级"))
                        .clicked()
                    {
                        let parent = self
                            .location
                            .path
                            .rsplit_once('/')
                            .map_or("/", |(parent, _)| parent);
                        navigate = Some(if parent.is_empty() {
                            "/".into()
                        } else {
                            parent.into()
                        });
                    }
                    if ui.button("刷新").clicked() {
                        navigate = Some(self.location.path.clone());
                    }
                    ui.monospace(&self.location.path);
                    if self.has_password || !self.password.is_empty() {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.password)
                                .password(true)
                                .hint_text("频道密码")
                                .desired_width(120.),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("上传文件…").clicked() {
                            self.form = Some(Form {
                                operation: Operation::Upload,
                                value: String::new(),
                            });
                        }
                        if ui.button("新建文件夹…").clicked() {
                            self.form = Some(Form {
                                operation: Operation::Folder,
                                value: String::new(),
                            });
                        }
                    });
                });
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("file-entries")
                .max_height(230.)
                .show_rows(ui, 24., self.entries.len(), |ui, rows| {
                    for i in rows {
                        let entry = &self.entries[i];
                        let label = if entry.is_file {
                            format!("📄 {}  （{} 字节）", entry.name, entry.size)
                        } else {
                            format!("📁 {}", entry.name)
                        };
                        let row = ui
                            .selectable_label(self.selection == Some(i), label)
                            .on_hover_text(&entry.modified);
                        if !busy && self.form.is_none() && row.clicked() {
                            self.selection = Some(i);
                        }
                        if !busy && self.form.is_none() && row.double_clicked() {
                            if entry.is_file {
                                self.form = Some(download_form(entry));
                            } else {
                                match child_path(&self.location.path, &entry.name) {
                                    Ok(path) => navigate = Some(path),
                                    Err(error) => self.status = error,
                                }
                            }
                        }
                    }
                });
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                let entry = self.selection.and_then(|i| self.entries.get(i));
                ui.add_enabled_ui(!busy && entry.is_some() && self.form.is_none(), |ui| {
                    if let Some(entry) = entry {
                        if entry.is_file && ui.button("下载…").clicked() {
                            self.form = Some(download_form(entry));
                        }
                        if ui.button("重命名…").clicked() {
                            self.form = Some(Form {
                                operation: Operation::Rename(entry.name.clone()),
                                value: entry.name.clone(),
                            });
                        }
                        if ui.button("删除…").clicked() {
                            self.form = Some(Form {
                                operation: Operation::Delete(entry.name.clone()),
                                value: String::new(),
                            });
                        }
                    }
                });
                if !self.status.is_empty() {
                    ui.weak(&self.status);
                }
            });
            if let Some(progress) = self.running.as_ref().and_then(|job| job.progress.as_ref()) {
                let progress = *progress.borrow();
                let fraction = if progress.total == 0 {
                    0.
                } else {
                    progress.bytes as f32 / progress.total as f32
                };
                ui.horizontal(|ui| {
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .desired_width(240.)
                            .text(format!("{}/{} 字节", progress.bytes, progress.total)),
                    );
                    if ui.button("取消传输").clicked() {
                        self.running = None;
                        self.status = "传输已取消，上传残留可在刷新后检查".into();
                    }
                });
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            self.inline_form(ui, ctx, backend);
            if !busy && self.form.is_none() && ui.rect_contains_pointer(ui.max_rect()) {
                dropped = ctx.input(|i| {
                    i.raw
                        .dropped_files
                        .first()
                        .map(|file| file.path().to_path_buf())
                });
            }
        });
        if let Some(path) = dropped {
            self.form = Some(Form {
                operation: Operation::Upload,
                value: path.to_string_lossy().into_owned(),
            });
        }
        if let Some(path) = navigate {
            self.list(backend, ctx, path);
        }
        open
    }

    fn inline_form(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, backend: &Backend) {
        let Some(mut form) = self.form.take() else {
            return;
        };
        ui.separator();
        let mut submit = false;
        let mut cancel = false;
        ui.horizontal_wrapped(|ui| {
            match &form.operation {
                Operation::Folder => {
                    ui.strong("新建文件夹：");
                    ui.add(
                        egui::TextEdit::singleline(&mut form.value)
                            .hint_text("文件夹名称")
                            .desired_width(220.),
                    );
                }
                Operation::Rename(name) => {
                    ui.strong(format!("重命名「{name}」："));
                    ui.add(
                        egui::TextEdit::singleline(&mut form.value)
                            .hint_text("新名称")
                            .desired_width(220.),
                    );
                }
                Operation::Delete(name) => {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        format!("确认从服务器删除「{name}」？"),
                    );
                }
                Operation::Upload => {
                    ui.strong("上传到当前目录：");
                    ui.add(
                        egui::TextEdit::singleline(&mut form.value)
                            .hint_text("本地完整文件路径（支持拖入文件）")
                            .desired_width(280.),
                    );
                }
                Operation::Download(name) => {
                    ui.strong(format!("下载「{name}」到："));
                    ui.add(
                        egui::TextEdit::singleline(&mut form.value)
                            .hint_text("本地保存完整路径")
                            .desired_width(280.),
                    );
                }
            }
            let valid =
                matches!(form.operation, Operation::Delete(_)) || !form.value.trim().is_empty();
            if ui.add_enabled(valid, egui::Button::new("确定")).clicked() {
                submit = true;
            }
            if ui.button("取消").clicked() {
                cancel = true;
            }
        });
        if submit {
            if let Err(error) = self.submit(backend, ctx, form) {
                self.status = error;
            }
        } else if !cancel {
            self.form = Some(form);
        }
    }

    fn submit(&mut self, backend: &Backend, ctx: &egui::Context, form: Form) -> Result<(), String> {
        let session = backend.session.clone();
        let path = &self.location.path;
        let value = &form.value;
        match form.operation {
            Operation::Upload => {
                let source = PathBuf::from(value);
                let name = source
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or("本地文件名无效")?;
                let location = self.location(child_path(path, name)?);
                let (tx, rx) = watch::channel(Progress::default());
                self.start(
                    backend,
                    ctx,
                    async move { session.upload(location, source, tx).await.map(|()| None) },
                    Some(rx),
                    "上传数据已发送，目录已刷新",
                    true,
                );
            }
            Operation::Download(name) => {
                let location = self.location(child_path(path, &name)?);
                let destination = PathBuf::from(value);
                let (tx, rx) = watch::channel(Progress::default());
                self.start(
                    backend,
                    ctx,
                    async move {
                        session
                            .download(location, destination, tx)
                            .await
                            .map(|()| None)
                    },
                    Some(rx),
                    "下载完成",
                    false,
                );
            }
            operation => {
                let (location, action) = match operation {
                    Operation::Folder => (child_path(path, value)?, Mutation::CreateDirectory),
                    Operation::Rename(name) => (
                        child_path(path, &name)?,
                        Mutation::Rename {
                            destination: child_path(path, value)?,
                        },
                    ),
                    Operation::Delete(name) => (child_path(path, &name)?, Mutation::Delete),
                    _ => unreachable!(),
                };
                let location = self.location(location);
                self.start(
                    backend,
                    ctx,
                    async move { session.file_mutation(location, action).await.map(|()| None) },
                    None,
                    "服务器已确认，目录已刷新",
                    true,
                );
            }
        }
        Ok(())
    }
}

fn download_form(entry: &Entry) -> Form {
    let value = dirs::download_dir()
        .filter(|_| !entry.name.contains([':', '*', '?', '"', '<', '>', '|']))
        .map(|path| path.join(&entry.name).to_string_lossy().into_owned())
        .unwrap_or_default();
    Form {
        operation: Operation::Download(entry.name.clone()),
        value,
    }
}
