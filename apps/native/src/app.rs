use crate::icons::{Glyph, Icons};
use crate::{
    backend::{Backend, Event, IdentityChange},
    config::{self, Bookmark, Preferences},
    dialogs::Kind,
};
use congspeak_core::{
    AudioDevice, AudioSettings, Channel, ChatTarget, Client, ClientAction, ConnectRequest,
    ConnectionStatus, SessionEvent, Snapshot, TalkMode,
};
use congspeak_teamspeak::identities;
use eframe::egui::{self, Color32, RichText};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};
use uuid::Uuid;

struct Line {
    sender: String,
    text: String,
    id: Option<u32>,
    state: String,
}

struct Tab {
    key: String,
    title: String,
    target: Option<ChatTarget>,
    peer: Option<(u16, String)>,
    channel_id: Option<String>,
    draft: String,
    lines: VecDeque<Line>,
    unread: usize,
}

impl Tab {
    fn new(key: &str, title: &str, target: Option<ChatTarget>) -> Self {
        Self {
            key: key.into(),
            title: title.into(),
            target,
            peer: None,
            channel_id: None,
            draft: String::new(),
            lines: VecDeque::new(),
            unread: 0,
        }
    }
}

pub struct App {
    icons: Icons,
    pub backend: Backend,
    pub snapshot: Snapshot,
    pub preferences: Preferences,
    pub audio: AudioSettings,
    pub draft: AudioSettings,
    pub devices: Vec<AudioDevice>,
    pub settings_open: bool,
    pub settings_busy: bool,
    pub error: Option<String>,
    pub notice: String,
    pub temporary_identity: bool,
    pub identities_open: bool,
    pub identity_busy: bool,
    pub identity_details: Option<(Option<Uuid>, identities::Info)>,
    pub password: String,
    pub channel_password: String,
    pub connect_open: bool,
    pub bookmarks_open: bool,
    pub contacts_open: bool,
    pub details_open: bool,
    pub channel_search_open: bool,
    pub channel_filter: String,
    pub selected_channel: Option<String>,
    pub selected_client: Option<u16>,
    pub volumes: HashMap<u16, f32>,
    tabs: Vec<Tab>,
    active_tab: usize,
    chat_search_open: bool,
    chat_filter: String,
    pub network_open: bool,
    next_id: u32,
    pub pending: HashMap<u32, String>,
    pub form: Option<crate::dialogs::Form>,
    pub files: Option<crate::files::Browser>,
    pub ban_window: Option<crate::moderation::BanWindow>,
    hotkeys: Option<GlobalHotKeyManager>,
    hotkey: Option<HotKey>,
    pointer_ptt: bool,
    pub diagnostics: VecDeque<serde_json::Value>,
    diagnostic_at: Instant,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (preferences, audio, error) = config::load();
        cc.egui_ctx.set_theme(if preferences.light {
            egui::Theme::Light
        } else {
            egui::Theme::Dark
        });
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            cc.egui_ctx.style_mut_of(theme, |style| {
                let dark = theme == egui::Theme::Dark;
                let rgb = Color32::from_rgb;
                let panel = if dark {
                    rgb(27, 35, 42)
                } else {
                    rgb(245, 248, 249)
                };
                let surface = if dark {
                    rgb(37, 47, 54)
                } else {
                    rgb(255, 255, 255)
                };
                let selected = if dark {
                    rgb(37, 83, 80)
                } else {
                    rgb(195, 231, 222)
                };
                style.visuals.panel_fill = panel;
                style.visuals.window_fill = surface;
                style.visuals.extreme_bg_color = if dark {
                    rgb(19, 27, 33)
                } else {
                    rgb(233, 239, 241)
                };
                style.visuals.selection.bg_fill = selected;
                style.visuals.widgets.inactive.weak_bg_fill = surface;
                style.visuals.widgets.hovered.weak_bg_fill = selected;
                style.spacing.item_spacing = egui::vec2(6., 4.);
                style.spacing.button_padding = egui::vec2(8., 4.);
                style.spacing.window_margin = egui::Margin::same(10);
                style
                    .text_styles
                    .insert(egui::TextStyle::Heading, egui::FontId::proportional(20.));
            });
        }
        let backend = Backend::new(&cc.egui_ctx, audio.clone());
        let (session, ctx) = (backend.session.clone(), cc.egui_ctx.clone());
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            session.native_ptt(event.state == HotKeyState::Pressed);
            ctx.request_repaint();
        }));
        let mut app = Self {
            icons: Icons::load(&cc.egui_ctx),
            snapshot: backend.session.snapshot(),
            backend,
            preferences,
            audio: audio.clone(),
            draft: audio,
            devices: vec![],
            settings_open: false,
            settings_busy: false,
            error,
            notice: String::new(),
            temporary_identity: false,
            identities_open: false,
            identity_busy: false,
            identity_details: None,
            password: String::new(),
            channel_password: String::new(),
            connect_open: false,
            bookmarks_open: false,
            contacts_open: false,
            details_open: false,
            channel_search_open: false,
            channel_filter: String::new(),
            selected_channel: None,
            selected_client: None,
            volumes: HashMap::new(),
            tabs: Self::base_tabs(),
            active_tab: 0,
            chat_search_open: false,
            chat_filter: String::new(),
            network_open: false,
            next_id: 1,
            pending: HashMap::new(),
            form: None,
            files: None,
            ban_window: None,
            hotkeys: GlobalHotKeyManager::new().ok(),
            hotkey: None,
            pointer_ptt: false,
            diagnostics: VecDeque::new(),
            diagnostic_at: Instant::now(),
        };
        app.register_hotkey();
        app.apply_ignored();
        app
    }

    fn base_tabs() -> Vec<Tab> {
        vec![
            Tab::new("server", "服务器", Some(ChatTarget::Server)),
            Tab::new("poke", "事件", None),
        ]
    }

    pub fn connected(&self) -> bool {
        self.snapshot.status == ConnectionStatus::Connected
    }

    pub fn inactive(&self) -> bool {
        matches!(
            self.snapshot.status,
            ConnectionStatus::Disconnected | ConnectionStatus::Failed
        )
    }

    pub fn id(&mut self) -> u32 {
        let n = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        n
    }

    fn own_client(&self) -> Option<Client> {
        self.snapshot
            .clients
            .iter()
            .find(|c| Some(c.id) == self.snapshot.own_client_id)
            .cloned()
    }

    fn register_hotkey(&mut self) {
        if let Some(manager) = &self.hotkeys {
            let desired = self.audio.ptt_key.parse::<HotKey>();
            if self.hotkey.is_some() && desired.as_ref().ok() == self.hotkey.as_ref() {
                return;
            }
            if let Some(old) = self.hotkey.take() {
                self.backend.session.native_ptt(false);
                let _ = manager.unregister(old);
            }
            match desired.map_err(|e| e.to_string()).and_then(|key| {
                manager
                    .register(key)
                    .map(|_| key)
                    .map_err(|e| e.to_string())
            }) {
                Ok(key) => self.hotkey = Some(key),
                Err(e) => self.error = Some(format!("全局发言键不可用，可使用界面按钮：{e}")),
            }
        } else {
            self.notice = "全局快捷键不可用，可使用界面发言按钮".into();
        }
    }

    pub fn apply_audio(&mut self, settings: AudioSettings) {
        if !self.settings_busy {
            self.settings_busy = true;
            self.backend.apply(settings);
        }
    }

    pub fn call(&self, f: impl std::future::Future<Output = Result<(), String>> + Send + 'static) {
        self.backend.task(f);
    }

    pub fn action(&mut self, label: &str, action: ClientAction) {
        let id = self.id();
        self.submit_action(id, label, action);
    }

    pub fn submit_action(&mut self, id: u32, label: &str, action: ClientAction) {
        self.pending.insert(id, label.into());
        let session = self.backend.session.clone();
        let generation = self.snapshot.generation;
        let peer = action.target_client().and_then(|id| self.peer(id));
        if action.target_client().is_some() && peer.is_none() {
            self.pending.remove(&id);
            self.error = Some("目前无法确认目标用户身份，请重新选择用户".into());
            return;
        }
        self.backend.request(id, async move {
            session.action_confirmed(generation, peer, action).await
        });
    }

    pub fn run_identity(
        &mut self,
        operation: impl FnOnce() -> Result<IdentityChange, String> + Send + 'static,
    ) {
        if !self.inactive() || self.identity_busy {
            self.error = Some("请先断开连接并等待当前身份操作完成".into());
            return;
        }
        self.identity_busy = true;
        self.backend.identity(operation);
    }

    pub fn apply_ignored(&self) {
        let ignored = self
            .preferences
            .contacts
            .iter()
            .filter(|(_, c)| c.ignored)
            .map(|(uid, _)| uid.clone())
            .collect();
        let session = self.backend.session.clone();
        self.call(async move { session.ignore_uids(ignored).await });
    }

    pub fn save_contacts(&self) {
        self.apply_ignored();
        self.backend.save_preferences(self.preferences.clone());
    }

    fn append(&mut self, key: &str, line: Line) {
        let Some(index) = self.tabs.iter().position(|tab| tab.key == key) else {
            return;
        };
        let tab = &mut self.tabs[index];
        tab.lines.push_back(line);
        if tab.lines.len() > 500 {
            tab.lines.pop_front();
        }
        if index != self.active_tab {
            tab.unread += 1;
        }
    }

    pub fn peer(&self, id: u16) -> Option<(u16, String)> {
        self.snapshot
            .clients
            .iter()
            .find(|client| client.id == id)
            .and_then(|client| client.uid.clone())
            .map(|uid| (id, uid))
    }

    fn channel_tab(&mut self, id: &str) -> Option<usize> {
        let title = self
            .snapshot
            .channels
            .iter()
            .find(|channel| channel.id == id)
            .map_or_else(|| format!("频道 #{id}"), |channel| channel.name.clone());
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.channel_id.as_deref() == Some(id))
        {
            self.tabs[index].title = title;
            return Some(index);
        }
        let mut tab = Tab::new(&format!("channel:{id}"), &title, Some(ChatTarget::Channel));
        tab.channel_id = Some(id.into());
        if self
            .tabs
            .iter()
            .filter(|tab| tab.channel_id.is_some())
            .count()
            >= 24
        {
            let replacement = self.tabs.iter().enumerate().position(|(index, existing)| {
                index != self.active_tab
                    && existing.channel_id.is_some()
                    && existing.draft.is_empty()
                    && existing
                        .lines
                        .iter()
                        .all(|line| !line.id.is_some_and(|id| self.pending.contains_key(&id)))
            });
            let Some(index) = replacement else {
                self.error = Some("频道聊天标签已满，请关闭不使用的标签".into());
                return None;
            };
            self.tabs[index] = tab;
            return Some(index);
        }
        self.tabs.push(tab);
        Some(self.tabs.len() - 1)
    }

    fn open_channel_chat(&mut self, id: &str) {
        if let Some(index) = self.channel_tab(id) {
            self.activate_chat(index);
        }
    }

    fn activate_chat(&mut self, index: usize) {
        let tab = &mut self.tabs[index];
        tab.unread = 0;
        self.active_tab = index;
        self.chat_filter.clear();
        if tab.key == "poke" {
            return;
        }
        self.selected_client = tab.peer.as_ref().and_then(|(id, uid)| {
            self.snapshot
                .clients
                .iter()
                .any(|client| client.id == *id && client.uid.as_ref() == Some(uid))
                .then_some(*id)
        });
        self.selected_channel = tab.channel_id.clone();
    }

    fn open_current_channel_chat(&mut self) {
        if let Some(own) = self.own_client() {
            self.open_channel_chat(&own.channel_id);
        } else {
            self.active_tab = 0;
        }
    }

    fn open_client_chat(&mut self, client: &Client) {
        if Some(client.id) == self.snapshot.own_client_id {
            self.open_channel_chat(&client.channel_id);
        } else if let Some(index) = self.private(client.id, &client.name) {
            self.activate_chat(index);
        }
    }

    fn private(&mut self, id: u16, name: &str) -> Option<usize> {
        let Some((_, uid)) = self.peer(id) else {
            self.error = Some("目前无法确认该用户身份".into());
            return None;
        };
        self.private_for_uid(id, name, uid)
    }

    fn private_for_uid(&mut self, id: u16, name: &str, uid: String) -> Option<usize> {
        let key = format!("client:{id}:{uid}");
        if let Some(i) = self.tabs.iter().position(|t| t.key == key) {
            self.tabs[i].title = name.into();
            return Some(i);
        }
        if self.tabs.iter().filter(|tab| tab.peer.is_some()).count() >= 16 {
            self.error = Some("私聊标签页已达上限，请先关闭不使用的私聊".into());
            return None;
        }
        let mut tab = Tab::new(&key, name, Some(ChatTarget::Client { client_id: id }));
        tab.peer = Some((id, uid));
        self.tabs.push(tab);
        Some(self.tabs.len() - 1)
    }

    fn receive(&mut self) {
        let snapshot = self.backend.session.snapshot();
        if snapshot.generation != self.snapshot.generation
            || (self.connected() && snapshot.status != ConnectionStatus::Connected)
        {
            self.form = None;
            self.files = None;
            self.ban_window = None;
            self.selected_client = None;
            self.selected_channel = None;
            self.pointer_ptt = false;
        }
        if snapshot.generation != self.snapshot.generation {
            self.tabs = Self::base_tabs();
            self.active_tab = 0;
            self.volumes.clear();
            self.pending.clear();
            self.selected_client = None;
        }
        self.volumes.retain(|id, _| {
            snapshot
                .clients
                .iter()
                .find(|c| c.id == *id)
                .is_some_and(|client| {
                    self.snapshot
                        .clients
                        .iter()
                        .any(|previous| previous.id == *id && previous.uid == client.uid)
                })
        });
        let just_connected = (self.snapshot.generation != snapshot.generation
            || self.snapshot.status != ConnectionStatus::Connected)
            && snapshot.status == ConnectionStatus::Connected;
        let previous_channel = self.own_client().map(|own| own.channel_id);
        self.snapshot = snapshot;
        if just_connected {
            self.backend.save_preferences(self.preferences.clone());
            self.connect_open = false;
            self.open_current_channel_chat();
            self.action("订阅可见频道", ClientAction::Subscribe { enabled: true });
        } else if previous_channel.is_some()
            && self.tabs[self.active_tab].channel_id.as_ref() == previous_channel.as_ref()
            && let Some(own) = self.own_client()
            && Some(&own.channel_id) != previous_channel.as_ref()
        {
            self.open_channel_chat(&own.channel_id);
        }
        while let Ok(event) = self.backend.events.try_recv() {
            match event {
                Event::Error(e) => self.error = Some(e),
                Event::Settings(result) => {
                    self.settings_busy = false;
                    self.audio = self.backend.session.audio_settings();
                    match result {
                        Ok(s) => {
                            self.audio = s;
                            self.draft = self.audio.clone();
                            self.notice = "音频设置已保存".into();
                            self.register_hotkey();
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
                Event::Devices(result) => match result {
                    Ok(d) => self.devices = d,
                    Err(e) => self.error = Some(e),
                },
                Event::Identity(result) => {
                    self.identity_busy = false;
                    match result {
                        Ok(IdentityChange::Stored(id, name)) => {
                            self.preferences.identities.insert(id, name);
                            self.backend.save_preferences(self.preferences.clone());
                            self.notice = "身份已保存".into();
                        }
                        Ok(IdentityChange::Removed(id)) => {
                            self.preferences.identities.remove(&id);
                            self.identity_details = None;
                            self.backend.save_preferences(self.preferences.clone());
                            self.notice = "身份已删除".into();
                        }
                        Ok(IdentityChange::Info(id, info)) => {
                            self.identity_details = Some((id, info))
                        }
                        Ok(IdentityChange::Exported) => self.notice = "身份 INI 已导出".into(),
                        Err(error) => self.error = Some(error),
                    }
                }
                Event::Protocol(SessionEvent::Presence {
                    generation,
                    name,
                    channel,
                    joined,
                }) => {
                    if generation != self.snapshot.generation || !self.connected() {
                        continue;
                    }
                    let action = if joined {
                        "加入服务器"
                    } else {
                        "退出服务器"
                    };
                    let text = format!("{name} {action} · {channel}");
                    self.notice = text.clone();
                    self.append(
                        "poke",
                        Line {
                            sender: "动态".into(),
                            text,
                            id: None,
                            state: String::new(),
                        },
                    );
                    if !self.preferences.mute_presence {
                        let session = self.backend.session.clone();
                        self.call(async move { session.presence_tone(generation, joined).await });
                    }
                }
                Event::Protocol(SessionEvent::Message {
                    generation,
                    sender,
                    sender_id,
                    sender_uid,
                    channel_id,
                    mut target,
                    text,
                }) => {
                    if generation != self.snapshot.generation
                        || Some(sender_id) == self.snapshot.own_client_id
                    {
                        continue;
                    }
                    if target == "channel" {
                        let Some(id) = channel_id else { continue };
                        let Some(index) = self.channel_tab(&id) else {
                            continue;
                        };
                        target = self.tabs[index].key.clone();
                    } else if target.starts_with("client:") {
                        target = sender_uid
                            .and_then(|uid| self.private_for_uid(sender_id, &sender, uid))
                            .map_or_else(|| "poke".into(), |index| self.tabs[index].key.clone());
                    }
                    self.append(
                        &target,
                        Line {
                            sender,
                            text,
                            id: None,
                            state: String::new(),
                        },
                    );
                }
                Event::Protocol(SessionEvent::CommandResult { request_id, error }) => {
                    if let Some(window) = &mut self.ban_window {
                        window.complete(request_id, error.is_none());
                    }
                    for tab in &mut self.tabs {
                        for line in &mut tab.lines {
                            if line.id == Some(request_id) {
                                line.state = error
                                    .as_ref()
                                    .map(|e| format!("失败：{e}"))
                                    .unwrap_or_default();
                            }
                        }
                    }
                    if let Some(label) = self.pending.remove(&request_id) {
                        if let Some(e) = error {
                            self.error = Some(format!("{label}：{e}"));
                        } else {
                            self.notice = format!("{label}完成");
                        }
                    }
                }
                Event::Protocol(SessionEvent::Snapshot { .. }) => {}
            }
        }
        if self.diagnostic_at.elapsed() > Duration::from_secs(1) {
            self.diagnostic_at = Instant::now();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            self.diagnostics
                .push_back(serde_json::json!({ "time": now, "audio": self.snapshot.audio }));
            if self.diagnostics.len() > 120 {
                self.diagnostics.pop_front();
            }
        }
    }

    pub fn connect(&mut self) {
        let id = self.preferences.identity;
        if !self.temporary_identity
            && let Some(id) = id
            && !self.preferences.identities.contains_key(&id)
        {
            self.error = Some("此身份不可用，请先重新选择身份".into());
            return;
        }
        let session = self.backend.session.clone();
        let request = ConnectRequest {
            address: self.preferences.address.clone(),
            nickname: self.preferences.nickname.clone(),
            password: Some(std::mem::take(&mut self.password)).filter(|s| !s.is_empty()),
            channel: Some(self.preferences.channel.clone()).filter(|s| !s.is_empty()),
            channel_password: Some(std::mem::take(&mut self.channel_password))
                .filter(|s| !s.is_empty()),
        };
        let temporary = self.temporary_identity;
        self.call(async move {
            if temporary {
                session.connect_temporary(request).await
            } else if let Some(id) = id {
                session.connect_named(request, id).await
            } else {
                session.connect(request).await
            }
        });
    }

    pub fn join(&mut self, id: String) {
        self.open_channel_chat(&id);
        if self
            .snapshot
            .channels
            .iter()
            .any(|c| c.id == id && c.has_password)
        {
            self.prompt("加入加密频道", Kind::Join(id), &[("频道密码", "", true)]);
        } else {
            self.action(
                "切换频道",
                ClientAction::Join {
                    channel_id: id,
                    password: None,
                },
            );
        }
    }

    fn menubar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("menubar").resizable(false).show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("连接", |ui| {
                    if ui
                        .add_enabled(self.inactive(), egui::Button::new("连接服务器…"))
                        .clicked()
                    {
                        self.connect_open = true;
                        ui.close();
                    }
                    if ui
                        .add_enabled(!self.inactive(), egui::Button::new("断开当前连接"))
                        .clicked()
                    {
                        let session = self.backend.session.clone();
                        self.call(async move { session.disconnect().await });
                        ui.close();
                    }
                });

                ui.menu_button("收藏", |ui| {
                    if ui.button("管理收藏夹…").clicked() {
                        self.bookmarks_open = true;
                        ui.close();
                    }
                    if ui.button("添加当前服务器…").clicked() {
                        let b = Bookmark {
                            name: self.snapshot.server_name.clone().unwrap_or_default(),
                            address: self.preferences.address.clone(),
                            nickname: self.preferences.nickname.clone(),
                            channel: self.preferences.channel.clone(),
                            group: String::new(),
                            identity: self.preferences.identity,
                        };
                        self.edit_bookmark(None, b);
                        ui.close();
                    }
                    if !self.preferences.bookmarks.is_empty() {
                        ui.separator();
                        let list = self.preferences.bookmarks.clone();
                        for b in list {
                            let label = if b.group.is_empty() {
                                b.name.clone()
                            } else {
                                format!("{} / {}", b.group, b.name)
                            };
                            if ui
                                .add_enabled(self.inactive(), egui::Button::new(label))
                                .on_hover_text(&b.address)
                                .clicked()
                            {
                                self.preferences.address = b.address;
                                self.preferences.nickname = b.nickname;
                                self.preferences.channel = b.channel;
                                self.preferences.identity = b.identity;
                                self.temporary_identity = false;
                                self.connect_open = true;
                                ui.close();
                            }
                        }
                    }
                });

                ui.menu_button("自身", |ui| {
                    ui.add_enabled_ui(self.connected(), |ui| {
                        let own = self.own_client();
                        if ui.button("修改昵称…").clicked() {
                            let nick = self.preferences.nickname.clone();
                            self.prompt("修改昵称", Kind::Rename, &[("昵称", &nick, false)]);
                            ui.close();
                        }
                        if ui.button("编辑个人描述…").clicked() {
                            let desc = own.as_ref().map_or("", |c| c.description.as_str());
                            self.prompt(
                                "编辑个人描述",
                                Kind::ClientDescription,
                                &[("描述", desc, false)],
                            );
                            ui.close();
                        }
                        if own.as_ref().is_some_and(|c| c.away.is_some()) {
                            if ui.button("取消离开状态").clicked() {
                                self.action("取消离开", ClientAction::Away { message: None });
                                ui.close();
                            }
                        } else if ui.button("设置离开状态…").clicked() {
                            self.prompt("设置离开", Kind::Away, &[("离开消息", "", false)]);
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("请求发言权…").clicked() {
                            self.prompt("请求发言权", Kind::RequestTalk, &[("留言", "", false)]);
                            ui.close();
                        }
                        if ui.button("撤销发言请求").clicked() {
                            self.action(
                                "撤销发言请求",
                                ClientAction::RequestTalk { message: None },
                            );
                            ui.close();
                        }
                        ui.separator();
                        for (label, enabled) in [("订阅全部频道", true), ("取消全部订阅", false)]
                        {
                            if ui.button(label).clicked() {
                                self.action(label, ClientAction::Subscribe { enabled });
                                ui.close();
                            }
                        }
                        if ui.button("使用权限密钥…").clicked() {
                            self.prompt("使用权限密钥", Kind::Redeem, &[("密钥", "", true)]);
                            ui.close();
                        }
                    });
                });

                ui.menu_button("工具", |ui| {
                    if ui.button("本地身份…").clicked() {
                        self.identities_open = true;
                        ui.close();
                    }
                    if ui.button("联系人（好友/忽略）…").clicked() {
                        self.contacts_open = true;
                        ui.close();
                    }
                    if ui
                        .add_enabled(self.connected(), egui::Button::new("服务器封禁列表…"))
                        .clicked()
                    {
                        if self.ban_window.is_none() {
                            self.ban_window = Some(crate::moderation::BanWindow::new(
                                &self.backend,
                                ui.ctx(),
                                self.snapshot.generation,
                            ));
                        }
                        ui.close();
                    }
                    if ui.button("连接信息与统计…").clicked() {
                        self.network_open = true;
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("音频与语音设置…").clicked() {
                        self.settings_open = true;
                        self.draft = self.audio.clone();
                        self.backend.devices();
                        ui.close();
                    }
                });

                ui.menu_button("视图", |ui| {
                    if ui.button("竖向窗口").clicked() {
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                                440., 720.,
                            )));
                        ui.close();
                    }
                    let mut sound = !self.preferences.mute_presence;
                    if ui.checkbox(&mut sound, "加入与退出提示音").changed() {
                        self.preferences.mute_presence = !sound;
                        self.backend.save_preferences(self.preferences.clone());
                    }
                    ui.checkbox(&mut self.details_open, "显示详情");
                    if ui
                        .checkbox(&mut self.channel_search_open, "显示频道过滤栏")
                        .changed()
                        && !self.channel_search_open
                    {
                        self.channel_filter.clear();
                    }
                    if ui
                        .checkbox(&mut self.chat_search_open, "显示聊天搜索栏")
                        .changed()
                        && !self.chat_search_open
                    {
                        self.chat_filter.clear();
                    }
                    ui.separator();
                    if ui
                        .checkbox(&mut self.preferences.light, "浅色界面主题")
                        .changed()
                    {
                        ui.ctx().set_theme(if self.preferences.light {
                            egui::Theme::Light
                        } else {
                            egui::Theme::Dark
                        });
                        self.backend.save_preferences(self.preferences.clone());
                    }
                });
            });
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("voice-controls")
            .exact_size(64.)
            .resizable(false)
            .show(ui, |ui| {
                let mut next = self.audio.clone();
                let mut changed = false;
                ui.horizontal(|ui| {
                    let mic_label = if next.input_muted {
                        "麦克风：已静音"
                    } else {
                        "麦克风：开启"
                    };
                    changed = ui
                        .add_enabled(
                            !self.settings_busy,
                            self.icons
                                .button(
                                    ui,
                                    if next.input_muted {
                                        Glyph::MicOff
                                    } else {
                                        Glyph::Mic
                                    },
                                    mic_label,
                                )
                                .selected(!next.input_muted),
                        )
                        .clicked();
                    if changed {
                        next.input_muted = !next.input_muted;
                    }
                    let out_label = if next.output_muted {
                        "耳机：已静音"
                    } else {
                        "耳机：正常"
                    };
                    if ui
                        .add_enabled(
                            !self.settings_busy,
                            self.icons
                                .button(
                                    ui,
                                    if next.output_muted {
                                        Glyph::PhonesOff
                                    } else {
                                        Glyph::Phones
                                    },
                                    out_label,
                                )
                                .selected(next.output_muted),
                        )
                        .clicked()
                    {
                        next.output_muted = !next.output_muted;
                        changed = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::image(self.icons.image(
                                Glyph::Controls,
                                19.,
                                ui.visuals().text_color(),
                            )))
                            .on_hover_text("音频设置")
                            .clicked()
                        {
                            self.settings_open = true;
                            self.draft = self.audio.clone();
                            self.backend.devices();
                        }
                    });
                });
                ui.horizontal(|ui| {
                    let old = next.talk_mode.clone();
                    egui::ComboBox::from_id_salt("talk-mode")
                        .selected_text(talk_name(&next.talk_mode))
                        .show_ui(ui, |ui| {
                            for mode in [
                                TalkMode::PushToTalk,
                                TalkMode::VoiceActivation,
                                TalkMode::Continuous,
                            ] {
                                let name = talk_name(&mode);
                                ui.selectable_value(&mut next.talk_mode, mode, name);
                            }
                        });
                    if old != next.talk_mode || changed {
                        self.apply_audio(next);
                    }
                    if self.audio.talk_mode == TalkMode::PushToTalk {
                        let enabled = self.connected()
                            && !self.audio.input_muted
                            && !self.audio.output_muted
                            && !self.snapshot.audio.mic_test;
                        let ptt_btn =
                            egui::Button::new(format!("按住发言 ({})", self.audio.ptt_key))
                                .sense(egui::Sense::click_and_drag());
                        let held =
                            ui.add_enabled(enabled, ptt_btn).is_pointer_button_down_on() && enabled;
                        if held != self.pointer_ptt {
                            self.pointer_ptt = held;
                            let session = self.backend.session.clone();
                            self.call(async move { session.set_ptt(held).await });
                        }
                    }
                    if self.snapshot.audio.transmitting {
                        ui.add(
                            self.icons
                                .image(Glyph::Voice, 16., ui.visuals().hyperlink_color),
                        );
                        ui.label("正在发言");
                    }
                });
            });
    }

    fn statusbar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("statusbar")
            .resizable(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.available_width() < 660. {
                        ui.weak(status_name(&self.snapshot.status));
                        if ui
                            .small_button(self.snapshot.network.as_ref().map_or_else(
                                || "连接信息".into(),
                                |s| format!("{:.0} ms", s.round_trip_ms),
                            ))
                            .clicked()
                        {
                            self.network_open = true;
                        }
                        let note = if let Some(notice) = &self.snapshot.notice {
                            notice.as_str()
                        } else if self.notice.is_empty() {
                            self.snapshot
                                .audio
                                .send_block_reason
                                .as_deref()
                                .unwrap_or("")
                        } else {
                            &self.notice
                        };
                        ui.add(egui::Label::new(note).truncate())
                            .on_hover_text(note);
                        return;
                    }
                    let status = status_name(&self.snapshot.status);
                    match &self.snapshot.server_name {
                        Some(name) if self.connected() => {
                            ui.label(format!("● {status}：{name}"));
                        }
                        _ => {
                            ui.weak(status);
                        }
                    }
                    if let Some(notice) = &self.snapshot.notice {
                        ui.separator();
                        ui.weak(notice);
                    } else if !self.notice.is_empty() {
                        ui.separator();
                        ui.weak(&self.notice);
                    } else if !self.audio.input_muted
                        && self.connected()
                        && let Some(reason) = &self.snapshot.audio.send_block_reason
                    {
                        ui.separator();
                        ui.weak(reason);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let summary = if let Some(stats) = &self.snapshot.network {
                            format!(
                                "延迟 {:.0} ms · 丢包 {:.1}%",
                                stats.round_trip_ms,
                                stats.inbound_voice_loss * 100.
                            )
                        } else {
                            "连接信息".into()
                        };
                        if ui.small_button(summary).clicked() {
                            self.network_open = true;
                        }
                    });
                });
            });
    }

    fn channel_menu(&mut self, ui: &mut egui::Ui, channel: &Channel) {
        if ui.button("加入频道").clicked() {
            self.join(channel.id.clone());
            ui.close();
        }
        if ui.button("频道文件…").clicked() {
            self.files = Some(crate::files::Browser::new(
                channel,
                self.snapshot.generation,
            ));
            ui.close();
        }
        ui.separator();
        if ui.button("创建子频道…").clicked() {
            self.create_channel_form(&channel.id);
            ui.close();
        }
        if ui.button("编辑频道…").clicked() {
            self.edit_channel_form(channel);
            ui.close();
        }
        if ui.button("修改频道密码…").clicked() {
            self.prompt(
                format!("修改「{}」的频道密码", channel.name),
                Kind::ChannelPassword(channel.id.clone()),
                &[("新密码（留空清除）", "", true)],
            );
            ui.close();
        }
        if ui.button("移动或排序…").clicked() {
            self.prompt(
                "移动或排序频道",
                Kind::MoveChannel(channel.id.clone()),
                &[
                    ("目标上级", &channel.parent_id, false),
                    ("排在其后", &channel.previous_id, false),
                ],
            );
            ui.close();
        }
        for (label, enabled) in [("订阅频道", true), ("取消订阅", false)] {
            if ui.button(label).clicked() {
                self.action(
                    label,
                    ClientAction::SubscribeChannel {
                        channel_id: channel.id.clone(),
                        enabled,
                    },
                );
                ui.close();
            }
        }
        ui.separator();
        if ui.button("删除频道…").clicked() {
            self.prompt(
                format!("删除「{}」？", channel.name),
                Kind::DeleteChannel(channel.id.clone()),
                &[],
            );
            ui.close();
        }
    }

    fn client_actions(&mut self, ui: &mut egui::Ui, client: &Client) {
        let is_own = Some(client.id) == self.snapshot.own_client_id;
        if ui.button("私聊").clicked() {
            self.open_client_chat(client);
            ui.close();
        }
        if ui.button("Poke…").clicked() {
            self.prompt("Poke 用户", Kind::Poke(client.id), &[("消息", "", false)]);
            ui.close();
        }
        if ui.button("移动用户…").clicked() {
            let target = self
                .selected_channel
                .as_deref()
                .unwrap_or(&client.channel_id)
                .to_owned();
            self.prompt(
                "移动用户",
                Kind::Move(client.id),
                &[("目标频道 ID", &target, false), ("频道密码", "", true)],
            );
            ui.close();
        }
        if !is_own && let Some(uid) = client.uid.as_ref() {
            let note = self
                .preferences
                .contacts
                .get(uid)
                .map_or("", |c| c.note.as_str())
                .to_owned();
            ui.menu_button("联系人", |ui| {
                for (label, ignore) in [("设为好友", false), ("忽略此身份", true)] {
                    if ui.button(label).clicked() {
                        self.prompt(
                            label,
                            Kind::Contact(uid.clone(), ignore),
                            &[("名称", &client.name, false), ("备注", &note, false)],
                        );
                        ui.close();
                    }
                }
            });
        }
        if !is_own {
            ui.menu_button("发言权", |ui| {
                for (label, granted) in [("授予发言权", true), ("撤销发言权", false)] {
                    if ui.button(label).clicked() {
                        self.action(
                            label,
                            ClientAction::GrantTalk {
                                client_id: client.id,
                                granted,
                            },
                        );
                        ui.close();
                    }
                }
            });
        }
        ui.menu_button("管理与处罚", |ui| {
            for (server, label) in [(false, "踢出频道…"), (true, "踢出服务器…")] {
                if ui.button(label).clicked() {
                    self.prompt(
                        format!("确认将 {} {label}", client.name),
                        Kind::Kick(client.id, server),
                        &[("原因", "", false)],
                    );
                    ui.close();
                }
            }
            if !is_own {
                if ui.button("限时封禁…").clicked() {
                    self.prompt(
                        format!("限时封禁「{}」", client.name),
                        Kind::Ban(client.id),
                        &[("分钟", "60", false), ("原因", "", false)],
                    );
                    ui.close();
                }
                if let Some(uid) = &client.uid
                    && ui.button("投诉…").clicked()
                {
                    self.prompt(
                        format!("投诉「{}」", client.name),
                        Kind::Complaint(client.id, uid.clone()),
                        &[("原因", "", false)],
                    );
                    ui.close();
                }
            }
        });
    }

    fn server_root_menu(&mut self, ui: &mut egui::Ui) {
        if ui
            .add_enabled(self.connected(), egui::Button::new("创建根频道…"))
            .clicked()
        {
            self.create_channel_form("0");
            ui.close();
        }
        if ui
            .add_enabled(self.connected(), egui::Button::new("订阅全部频道"))
            .clicked()
        {
            self.action("订阅", ClientAction::Subscribe { enabled: true });
            ui.close();
        }
        if ui.button("搜索频道或用户…").clicked() {
            self.channel_search_open = true;
            ui.close();
        }
        ui.separator();
        if ui.button("连接信息…").clicked() {
            self.network_open = true;
            ui.close();
        }
    }

    fn channel_navigation(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("频道");
            if ui
                .add(
                    self.icons
                        .button(ui, Glyph::Members, "有人")
                        .selected(self.preferences.occupied_only),
                )
                .on_hover_text("仅列出有可见成员的频道")
                .clicked()
            {
                self.preferences.occupied_only = !self.preferences.occupied_only;
                self.backend.save_preferences(self.preferences.clone());
            }
            if ui
                .add(
                    self.icons
                        .button(ui, Glyph::Search, "查找")
                        .selected(self.channel_search_open),
                )
                .clicked()
            {
                self.channel_search_open = !self.channel_search_open;
                if !self.channel_search_open {
                    self.channel_filter.clear();
                }
            }
            if ui
                .add(
                    self.icons
                        .button(ui, Glyph::Details, "详情")
                        .selected(self.details_open),
                )
                .clicked()
            {
                self.details_open = !self.details_open;
            }
        });
        ui.separator();
        self.render_tree(ui);
    }

    fn render_tree(&mut self, ui: &mut egui::Ui) {
        if self.channel_search_open {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.channel_filter)
                        .hint_text("过滤频道或用户…")
                        .desired_width(ui.available_width() - 28.),
                );
                if ui.small_button("×").clicked() {
                    self.channel_search_open = false;
                    self.channel_filter.clear();
                }
            });
            ui.separator();
        }
        let filter = self.channel_filter.to_lowercase();
        let rows = channel_order(&self.snapshot);
        let own_channel_id = self.own_client().map(|c| c.channel_id);
        egui::ScrollArea::vertical()
            .id_salt("channel-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.inactive() {
                    let resp = ui.selectable_label(false, "🌐 未连接服务器（双击连接）");
                    if resp.double_clicked() || resp.clicked() {
                        self.connect_open = true;
                    }
                    return;
                }
                let server_title = self.snapshot.server_name.as_deref().unwrap_or("服务器");
                let card = egui::Frame::new()
                    .fill(if ui.visuals().dark_mode {
                        Color32::from_rgb(33, 59, 63)
                    } else {
                        Color32::from_rgb(221, 239, 235)
                    })
                    .corner_radius(7)
                    .inner_margin(10)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.add(self.icons.image(Glyph::Brand, 40., Color32::WHITE));
                            ui.vertical(|ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(server_title).size(17.).strong(),
                                    )
                                    .truncate(),
                                );
                                ui.weak(format!(
                                    "{} 个频道   ·   {} 位可见成员",
                                    self.snapshot.channels.len(),
                                    self.snapshot.clients.len()
                                ));
                            });
                        });
                    });
                let root_resp = ui.interact(
                    card.response.rect,
                    ui.scope_id().with("server-card"),
                    egui::Sense::click(),
                );
                ui.add_space(5.);
                if root_resp.clicked() {
                    self.activate_chat(0);
                    ui.ctx().request_repaint();
                }
                root_resp.context_menu(|ui| self.server_root_menu(ui));

                for (channel, depth) in rows {
                    let clients: Vec<_> = self
                        .snapshot
                        .clients
                        .iter()
                        .filter(|c| c.channel_id == channel.id)
                        .cloned()
                        .collect();
                    if self.preferences.occupied_only && clients.is_empty() {
                        continue;
                    }
                    if !filter.is_empty()
                        && !channel.name.to_lowercase().contains(&filter)
                        && !clients
                            .iter()
                            .any(|c| c.name.to_lowercase().contains(&filter))
                    {
                        continue;
                    }
                    ui.horizontal(|ui| {
                        ui.add_space((depth + 1).min(8) as f32 * 12.);
                        let selected = self.selected_client.is_none()
                            && self.selected_channel.as_deref() == Some(&channel.id);
                        let current = own_channel_id.as_deref() == Some(channel.id.as_str());
                        let count = if channel.subscribed || current {
                            format!("{} 人", clients.len())
                        } else if clients.is_empty() {
                            "未订阅".to_owned()
                        } else {
                            format!("可见 {} 人", clients.len())
                        };
                        let row_width = ui.available_width();
                        let response = ui
                            .allocate_ui_with_layout(
                                egui::vec2(row_width, 24.),
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.weak(count);
                                    ui.with_layout(
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            if current {
                                                let (bar, _) = ui.allocate_exact_size(
                                                    egui::vec2(3., 16.),
                                                    egui::Sense::hover(),
                                                );
                                                ui.painter().rect_filled(
                                                    bar,
                                                    2.,
                                                    ui.visuals().hyperlink_color,
                                                );
                                            }
                                            let glyph = if channel.has_password {
                                                Glyph::Lock
                                            } else {
                                                Glyph::Channel
                                            };
                                            let color = if current {
                                                ui.visuals().hyperlink_color
                                            } else {
                                                ui.visuals().weak_text_color()
                                            };
                                            ui.add(
                                                egui::Button::image_and_text(
                                                    self.icons.image(glyph, 16., color),
                                                    RichText::new(&channel.name).strong(),
                                                )
                                                .selected(selected)
                                                .truncate(),
                                            )
                                        },
                                    )
                                    .inner
                                },
                            )
                            .inner;
                        if response.clicked() {
                            self.open_channel_chat(&channel.id);
                            ui.ctx().request_repaint();
                        }
                        if response.double_clicked() {
                            self.join(channel.id.clone());
                        }
                        response.context_menu(|ui| self.channel_menu(ui, &channel));
                    });
                    for client in clients {
                        ui.horizontal(|ui| {
                            ui.add_space((depth + 2).min(9) as f32 * 12.);
                            let (avatar, _) =
                                ui.allocate_exact_size(egui::vec2(19., 19.), egui::Sense::hover());
                            let tint = match client.id % 4 {
                                0 => Color32::from_rgb(52, 94, 102),
                                1 => Color32::from_rgb(92, 74, 110),
                                2 => Color32::from_rgb(69, 88, 126),
                                _ => Color32::from_rgb(98, 83, 66),
                            };
                            ui.painter().circle_filled(avatar.center(), 9., tint);
                            if let Some(initial) = client.name.chars().next() {
                                ui.painter().text(
                                    avatar.center(),
                                    egui::Align2::CENTER_CENTER,
                                    initial,
                                    egui::FontId::proportional(11.),
                                    Color32::WHITE,
                                );
                            }
                            let indicator = if client.output_muted {
                                " · 耳机静音"
                            } else if client.input_muted {
                                " · 麦克风静音"
                            } else {
                                ""
                            };
                            let membership = client
                                .uid
                                .as_ref()
                                .and_then(|uid| self.preferences.contacts.get(uid))
                                .map_or("", |c| {
                                    if c.ignored {
                                        " [已忽略]"
                                    } else {
                                        " [好友]"
                                    }
                                });
                            let away = if client.away.is_some() {
                                " [离开]"
                            } else {
                                ""
                            };
                            let label = format!("{}{indicator}{membership}{away}", client.name);
                            let text = if Some(client.id) == self.snapshot.own_client_id {
                                RichText::new(label).strong()
                            } else {
                                RichText::new(label)
                            };
                            let speaking = self.snapshot.audio.speakers.contains(&client.id)
                                || (Some(client.id) == self.snapshot.own_client_id
                                    && self.snapshot.audio.transmitting);
                            let text = if speaking {
                                text.color(if ui.visuals().dark_mode {
                                    Color32::from_rgb(112, 222, 185)
                                } else {
                                    Color32::from_rgb(12, 111, 80)
                                })
                            } else {
                                text
                            };
                            if speaking || client.output_muted || client.input_muted {
                                let glyph = if speaking {
                                    Glyph::Voice
                                } else if client.output_muted {
                                    Glyph::PhonesOff
                                } else {
                                    Glyph::MicOff
                                };
                                ui.add(self.icons.image(glyph, 14., ui.visuals().text_color()));
                            }
                            let response = ui
                                .add(
                                    egui::Button::selectable(
                                        self.selected_client == Some(client.id),
                                        text,
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(&client.name);
                            if response.clicked() {
                                self.open_client_chat(&client);
                                ui.ctx().request_repaint();
                            }
                            response.context_menu(|ui| self.client_actions(ui, &client));
                        });
                    }
                }
            });
    }

    fn render_details(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("details-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some(client) = self
                    .selected_client
                    .and_then(|id| self.snapshot.clients.iter().find(|c| c.id == id))
                    .cloned()
                {
                    self.client_details(ui, &client);
                } else if let Some(channel) = self
                    .selected_channel
                    .as_ref()
                    .and_then(|id| self.snapshot.channels.iter().find(|c| &c.id == id))
                    .cloned()
                {
                    self.channel_details(ui, &channel);
                } else {
                    self.server_details(ui);
                }
            });
    }

    fn server_details(&mut self, ui: &mut egui::Ui) {
        if self.inactive() {
            ui.strong("未连接服务器");
            ui.separator();
            ui.weak("通过顶部「连接」或「收藏」菜单加入 TeamSpeak 3 服务器。");
            return;
        }
        ui.strong(self.snapshot.server_name.as_deref().unwrap_or("当前服务器"));
        ui.separator();
        egui::Grid::new("server-info-grid")
            .num_columns(2)
            .spacing([12., 6.])
            .show(ui, |ui| {
                ui.weak("连接状态");
                ui.label(status_name(&self.snapshot.status));
                ui.end_row();
                ui.weak("服务器地址");
                ui.label(&self.preferences.address);
                ui.end_row();
                ui.weak("频道 / 在线");
                ui.label(format!(
                    "{} 个频道 · {} 位在线用户",
                    self.snapshot.channels.len(),
                    self.snapshot.clients.len()
                ));
                ui.end_row();
                if let Some(stats) = &self.snapshot.network {
                    ui.weak("延迟与波动");
                    ui.label(format!(
                        "{:.0} ms ± {:.0} ms",
                        stats.round_trip_ms, stats.variation_ms
                    ));
                    ui.end_row();
                    ui.weak("接收丢包");
                    ui.label(format!(
                        "语音 {:.2}% · 总计 {:.2}%",
                        stats.inbound_voice_loss * 100.,
                        stats.inbound_loss * 100.
                    ));
                    ui.end_row();
                    ui.weak("累计流量");
                    ui.label(format!(
                        "收 {} KiB / 发 {} KiB",
                        stats.inbound_bytes / 1024,
                        stats.outbound_bytes / 1024
                    ));
                    ui.end_row();
                }
            });
    }

    fn client_details(&mut self, ui: &mut egui::Ui, client: &Client) {
        let is_own = Some(client.id) == self.snapshot.own_client_id;
        ui.strong(&client.name);
        ui.separator();
        egui::Grid::new("client-info-grid")
            .num_columns(2)
            .spacing([12., 6.])
            .show(ui, |ui| {
                ui.weak("用户 ID");
                ui.label(format!("#{}", client.id));
                ui.end_row();
                ui.weak("语音状态");
                let mut states = vec![if client.input_muted {
                    "麦克风静音"
                } else {
                    "麦克风开启"
                }];
                if client.output_muted {
                    states.push("耳机静音");
                }
                ui.label(states.join(" · "));
                ui.end_row();
                if let Some(away) = &client.away {
                    ui.weak("离开消息");
                    ui.label(away);
                    ui.end_row();
                }
                if let Some(uid) = client.uid.as_ref() {
                    ui.weak("唯一标识");
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(uid).monospace().small());
                        if ui.small_button("复制").clicked() {
                            ui.ctx().copy_text(uid.clone());
                        }
                    });
                    ui.end_row();
                    if !is_own && let Some(contact) = self.preferences.contacts.get(uid) {
                        ui.weak("联系人");
                        ui.label(if contact.ignored {
                            "已忽略"
                        } else {
                            "好友"
                        });
                        ui.end_row();
                        if !contact.note.is_empty() {
                            ui.weak("备注");
                            ui.label(&contact.note);
                            ui.end_row();
                        }
                    }
                }
                if !client.description.is_empty() {
                    ui.weak("描述");
                    ui.label(&client.description);
                    ui.end_row();
                }
            });
        if !is_own {
            ui.separator();
            let mut volume = self.volumes.get(&client.id).copied().unwrap_or(1.);
            if ui
                .add(egui::Slider::new(&mut volume, 0.0..=2.0).text("个人音量"))
                .changed()
            {
                self.volumes.insert(client.id, volume);
                let session = self.backend.session.clone();
                let id = client.id;
                let generation = self.snapshot.generation;
                let uid = client.uid.clone();
                self.call(
                    async move { session.set_user_volume(generation, id, uid, volume).await },
                );
            }
        }
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            if ui.button("私聊").clicked() {
                self.open_client_chat(client);
            }
            if ui.button("Poke…").clicked() {
                self.prompt("Poke 用户", Kind::Poke(client.id), &[("消息", "", false)]);
            }
            ui.menu_button("更多操作", |ui| self.client_actions(ui, client));
        });
    }

    fn channel_details(&mut self, ui: &mut egui::Ui, channel: &Channel) {
        ui.strong(&channel.name);
        ui.separator();
        egui::Grid::new("channel-info-grid")
            .num_columns(2)
            .spacing([12., 6.])
            .show(ui, |ui| {
                ui.weak("频道 ID");
                ui.label(format!("#{}", channel.id));
                ui.end_row();
                ui.weak("语音编码");
                ui.label(&channel.codec);
                ui.end_row();
                ui.weak("访问权限");
                ui.label(if channel.has_password {
                    "需密码"
                } else {
                    "公开"
                });
                ui.end_row();
                if let Some(topic) = &channel.topic
                    && !topic.is_empty()
                {
                    ui.weak("主题");
                    ui.label(topic);
                    ui.end_row();
                }
            });
        if let Some(desc) = &channel.description
            && !desc.is_empty()
        {
            ui.separator();
            ui.label(desc);
        }
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            let connected = self.connected();
            if ui
                .add_enabled(connected, egui::Button::new("加入频道"))
                .clicked()
            {
                self.join(channel.id.clone());
            }
            if ui
                .add_enabled(connected, egui::Button::new("频道文件…"))
                .clicked()
            {
                self.files = Some(crate::files::Browser::new(
                    channel,
                    self.snapshot.generation,
                ));
            }
            ui.menu_button("管理频道", |ui| self.channel_menu(ui, channel));
        });
    }

    fn chat_content(&mut self, ui: &mut egui::Ui, compact: bool) {
        let current_channel = self.own_client().map(|own| own.channel_id);
        let mut selected_tab = None;
        ui.horizontal(|ui| {
            if self.active_tab >= 2
                && (self.tabs[self.active_tab].channel_id.is_none()
                    || self.tabs[self.active_tab].channel_id != current_channel)
                && ui
                    .small_button("×")
                    .on_hover_text("关闭当前聊天标签")
                    .clicked()
            {
                self.tabs.remove(self.active_tab);
                self.active_tab = 0;
                self.open_current_channel_chat();
            }
            egui::ScrollArea::horizontal()
                .id_salt("conversation-tabs")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (index, tab) in self.tabs.iter().enumerate() {
                            let label = if tab.unread > 0 {
                                format!("{} ({})", tab.title, tab.unread)
                            } else {
                                tab.title.clone()
                            };
                            if ui
                                .selectable_label(self.active_tab == index, label)
                                .clicked()
                            {
                                selected_tab = Some(index);
                            }
                        }
                    });
                });
        });
        if let Some(index) = selected_tab {
            self.activate_chat(index);
        }
        let title = &self.tabs[self.active_tab].title;
        if compact {
            ui.add(egui::Label::new(RichText::new(title).strong()).truncate());
        } else {
            ui.heading(title);
            ui.add_space(4.);
        }
        if !compact || self.chat_search_open {
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.selectable_label(self.chat_search_open, "搜索").clicked() {
                        self.chat_search_open = !self.chat_search_open;
                        if !self.chat_search_open {
                            self.chat_filter.clear();
                        }
                    }
                    if self.chat_search_open {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.chat_filter)
                                .hint_text("过滤消息…")
                                .desired_width(140.),
                        );
                    }
                });
            });
        }
        ui.separator();
        let filter = if self.chat_search_open {
            self.chat_filter.to_lowercase()
        } else {
            String::new()
        };
        let available = (ui.available_height() - 34.).max(32.);
        let tab = &mut self.tabs[self.active_tab];
        tab.unread = 0;
        egui::ScrollArea::vertical()
            .id_salt(("chat", &tab.key))
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .max_height(available)
            .min_scrolled_height(available)
            .show(ui, |ui| {
                let mut matches = 0;
                for line in &tab.lines {
                    if !filter.is_empty()
                        && !line.text.to_lowercase().contains(&filter)
                        && !line.sender.to_lowercase().contains(&filter)
                    {
                        continue;
                    }
                    matches += 1;
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(format!("{}:", line.sender));
                        ui.label(&line.text);
                        if !line.state.is_empty() {
                            ui.weak(&line.state);
                        }
                    });
                }
                if !filter.is_empty() && matches == 0 {
                    ui.weak("无匹配聊天记录");
                }
            });
        let tab = &self.tabs[self.active_tab];
        let peer_live = tab.peer.as_ref().is_none_or(|(id, uid)| {
            self.snapshot
                .clients
                .iter()
                .any(|c| c.id == *id && c.uid.as_ref() == Some(uid))
        });
        let in_channel = tab
            .channel_id
            .as_ref()
            .is_none_or(|id| Some(id) == current_channel.as_ref());
        let enabled = self.connected() && tab.target.is_some() && peer_live && in_channel;
        let hint = if !in_channel {
            "加入此频道后才能参与聊天"
        } else if !peer_live {
            "此用户已离开或不可见"
        } else if tab.target.is_none() {
            "事件记录"
        } else {
            "输入消息，回车发送"
        };
        let composer_id = ui.make_persistent_id(("compose", &tab.key));
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal(|ui| {
                let response = ui.add_sized(
                    [ui.available_width() - 56., 24.],
                    egui::TextEdit::singleline(&mut self.tabs[self.active_tab].draft)
                        .id(composer_id)
                        .hint_text(hint),
                );
                let send_clicked = ui.button("发送").clicked();
                if enabled
                    && (response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        || send_clicked)
                    && !self.tabs[self.active_tab].draft.trim().is_empty()
                {
                    let text = std::mem::take(&mut self.tabs[self.active_tab].draft);
                    let id = self.id();
                    let target = self.tabs[self.active_tab].target.clone().unwrap();
                    let key = self.tabs[self.active_tab].key.clone();
                    let peer = self.tabs[self.active_tab].peer.clone();
                    let channel_id = self.tabs[self.active_tab].channel_id.clone();
                    let generation = self.snapshot.generation;
                    self.append(
                        &key,
                        Line {
                            sender: self.preferences.nickname.clone(),
                            text: text.clone(),
                            id: Some(id),
                            state: "等待确认".into(),
                        },
                    );
                    self.pending.insert(id, "发送消息".into());
                    let session = self.backend.session.clone();
                    self.backend.request(id, async move {
                        if let Some(channel_id) = channel_id {
                            session
                                .chat_channel_confirmed(generation, channel_id, text)
                                .await
                        } else {
                            session.chat_confirmed(generation, peer, target, text).await
                        }
                    });
                    response.request_focus();
                }
            });
        });
    }
}

impl eframe::App for App {
    fn logic(&mut self, _: &egui::Context, _: &mut eframe::Frame) {
        self.receive();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.menubar(ui);

        if let Some(error) = self
            .error
            .clone()
            .or_else(|| self.snapshot.last_error.clone())
        {
            egui::Panel::top("error-banner")
                .resizable(false)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(Color32::LIGHT_RED, error);
                        if self.error.is_some() && ui.small_button("关闭").clicked() {
                            self.error = None;
                        }
                    });
                });
        }
        self.statusbar(ui);
        self.toolbar(ui);
        if self.inactive() {
            egui::CentralPanel::default().show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(20.);
                    ui.horizontal(|ui| {
                        ui.add(self.icons.image(Glyph::Brand, 42., Color32::WHITE));
                        ui.heading("CongSpeak");
                    });
                    ui.add_space(8.);
                    if !self.preferences.address.is_empty() {
                        ui.weak("上次连接");
                        ui.label(&self.preferences.address);
                    }
                    if ui
                        .add(
                            egui::Button::new(RichText::new("连接服务器…").strong())
                                .min_size(egui::vec2(148., 33.)),
                        )
                        .clicked()
                    {
                        self.connect_open = true;
                    }
                    ui.add_space(18.);
                    ui.horizontal(|ui| {
                        ui.strong("服务器收藏");
                        if ui.small_button("管理…").clicked() {
                            self.bookmarks_open = true;
                        }
                    });
                    ui.separator();
                    if self.preferences.bookmarks.is_empty() {
                        ui.weak("收藏常用服务器后，可从这里加入。");
                    }
                    for b in &self.preferences.bookmarks {
                        ui.horizontal_wrapped(|ui| {
                            if ui.button(&b.name).clicked() {
                                self.preferences.address = b.address.clone();
                                self.preferences.nickname = b.nickname.clone();
                                self.preferences.channel = b.channel.clone();
                                self.preferences.identity = b.identity;
                                self.temporary_identity = false;
                                self.connect_open = true;
                            }
                            ui.weak(&b.address);
                        });
                    }
                });
            });
        } else {
            let portrait = ui.available_width() < 700.;
            if portrait {
                egui::Panel::bottom("portrait-chat")
                    .resizable(true)
                    .default_size(176.)
                    .min_size(118.)
                    .max_size(ui.available_height() * 0.55)
                    .show(ui, |ui| self.chat_content(ui, true));
                egui::CentralPanel::default().show(ui, |ui| self.channel_navigation(ui));
            } else {
                egui::Panel::left("channel-navigation")
                    .default_size(258.)
                    .min_size(182.)
                    .max_size(ui.available_width() * 0.45)
                    .show(ui, |ui| self.channel_navigation(ui));
            }
            if self.details_open && !portrait && ui.available_width() >= 700. {
                egui::Panel::right("selection-info")
                    .default_size(245.)
                    .min_size(200.)
                    .max_size(ui.available_width() * 0.4)
                    .show(ui, |ui| self.render_details(ui));
            } else if self.details_open {
                let mut open = true;
                egui::Window::new("详情")
                    .open(&mut open)
                    .default_width(280.)
                    .show(&ctx, |ui| self.render_details(ui));
                self.details_open = open;
            }
            if !portrait {
                egui::CentralPanel::default().show(ui, |ui| self.chat_content(ui, false));
            }
        }

        self.connection_window(&ctx);
        self.bookmarks_window(&ctx);
        self.identities_window(&ctx);
        self.contacts_window(&ctx);
        if let Some(window) = &mut self.ban_window {
            let generation = window.generation;
            let (open, removal) = window.show(&self.backend, &ctx);
            if !open {
                self.ban_window = None;
            }
            if let Some((ban_id, target)) = removal {
                self.prompt(
                    format!("解除 #{ban_id}「{target}」的封禁？"),
                    Kind::RemoveBan { generation, ban_id },
                    &[],
                );
            }
        }
        self.form_window(&ctx);
        if let Some(files) = &mut self.files
            && !files.show(&ctx, &self.backend)
        {
            self.files = None;
        }
        self.settings(&ctx);
        self.network_window(&ctx);
        if self.settings_open || (!self.audio.input_muted && self.connected()) {
            ctx.request_repaint_after(Duration::from_millis(125));
        }
    }

    fn on_exit(&mut self, _: Option<&eframe::glow::Context>) {
        let session = self.backend.session.clone();
        let save = self.backend.flush_preferences();
        self.backend.runtime().block_on(async move {
            let _ = tokio::time::timeout(Duration::from_secs(3), async move {
                session.native_ptt(false);
                let _ = tokio::join!(session.disconnect(), save);
            })
            .await;
        });
    }
}

pub fn talk_name(mode: &TalkMode) -> &'static str {
    match mode {
        TalkMode::PushToTalk => "按键讲话",
        TalkMode::VoiceActivation => "语音激活",
        TalkMode::Continuous => "持续发送",
    }
}

pub fn status_name(s: &ConnectionStatus) -> &'static str {
    match s {
        ConnectionStatus::Disconnected => "未连接",
        ConnectionStatus::Connecting => "正在连接",
        ConnectionStatus::Synchronizing => "同步频道",
        ConnectionStatus::Connected => "已连接",
        ConnectionStatus::Reconnecting => "正在重连",
        ConnectionStatus::Disconnecting => "正在断开",
        ConnectionStatus::Failed => "连接失败",
    }
}

fn channel_order(snapshot: &Snapshot) -> Vec<(Channel, usize)> {
    fn walk(
        parent: &str,
        depth: usize,
        channels: &[Channel],
        seen: &mut HashSet<String>,
        out: &mut Vec<(Channel, usize)>,
    ) {
        if depth > 64 {
            return;
        }
        let mut remaining: Vec<_> = channels
            .iter()
            .filter(|c| c.parent_id == parent && !seen.contains(&c.id))
            .cloned()
            .collect();
        let mut previous = "0".to_owned();
        while !remaining.is_empty() {
            let index = remaining
                .iter()
                .position(|c| c.previous_id == previous)
                .unwrap_or(0);
            let channel = remaining.remove(index);
            if !seen.insert(channel.id.clone()) {
                continue;
            }
            previous = channel.id.clone();
            out.push((channel.clone(), depth));
            walk(&channel.id, depth + 1, channels, seen, out);
        }
    }
    let mut out = vec![];
    let mut seen = HashSet::new();
    walk("0", 0, &snapshot.channels, &mut seen, &mut out);
    for c in &snapshot.channels {
        if seen.insert(c.id.clone()) {
            out.push((c.clone(), 0));
            walk(&c.id, 1, &snapshot.channels, &mut seen, &mut out);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn channel_tree_uses_predecessor_order_and_keeps_orphans_and_cycles_bounded() {
        fn channel(id: &str, parent: &str, previous: &str) -> Channel {
            Channel {
                id: id.into(),
                parent_id: parent.into(),
                previous_id: previous.into(),
                name: id.into(),
                codec: "OpusVoice".into(),
                topic: None,
                description: None,
                has_password: false,
                subscribed: false,
            }
        }
        let snapshot = Snapshot {
            channels: vec![
                channel("2", "0", "1"),
                channel("1", "0", "0"),
                channel("3", "1", "0"),
                channel("9", "404", "0"),
                channel("7", "8", "0"),
                channel("8", "7", "0"),
            ],
            ..Default::default()
        };
        let rows = channel_order(&snapshot);
        assert_eq!(
            &rows[..3]
                .iter()
                .map(|(c, d)| (c.id.as_str(), *d))
                .collect::<Vec<_>>(),
            &[("1", 0), ("3", 1), ("2", 0)]
        );
        assert_eq!(rows.len(), 6);
    }
}
