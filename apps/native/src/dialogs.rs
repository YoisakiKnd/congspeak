use crate::{
    app::{App, status_name},
    backend::IdentityChange,
    config::{Bookmark, Contact},
};
use congspeak_core::{Channel, ChannelLifetime, ClientAction};
use congspeak_teamspeak::{IdentityMode, identities};
use eframe::egui::{self, Color32};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
use uuid::Uuid;

#[derive(Clone)]
pub enum Kind {
    Join(String),
    Rename,
    Away,
    Poke(u16),
    Move(u16),
    Kick(u16, bool),
    Edit(String),
    CreateChannel(String),
    MoveChannel(String),
    DeleteChannel(String),
    ChannelPassword(String),
    ClientDescription,
    RequestTalk,
    Ban(u16),
    RemoveBan { generation: u32, ban_id: u32 },
    Complaint(u16, String),
    Contact(String, bool),
    Redeem,
    Bookmark(Option<usize>),
    DeleteBookmark(usize),
    ImportBookmarks,
    NewIdentity,
    RenameIdentity(Uuid),
    ImportIdentity,
    ExportIdentity(Option<Uuid>, String),
    DeleteIdentity(Uuid),
}

impl Kind {
    fn client(&self) -> Option<u16> {
        match self {
            Self::Poke(id)
            | Self::Move(id)
            | Self::Kick(id, _)
            | Self::Ban(id)
            | Self::Complaint(id, _) => Some(*id),
            _ => None,
        }
    }
}

pub struct Form {
    pub title: String,
    pub kind: Kind,
    pub fields: Vec<(String, String, bool)>,
    pub lifetime: ChannelLifetime,
    pub identity: Option<Uuid>,
    peer: Option<(u16, String)>,
}

impl Form {
    pub fn new(title: impl Into<String>, kind: Kind, fields: &[(&str, &str, bool)]) -> Self {
        Self {
            title: title.into(),
            kind,
            lifetime: ChannelLifetime::Temporary,
            identity: None,
            peer: None,
            fields: fields
                .iter()
                .map(|(l, v, s)| ((*l).into(), (*v).into(), *s))
                .collect(),
        }
    }
}

impl App {
    pub fn prompt(&mut self, title: impl Into<String>, kind: Kind, fields: &[(&str, &str, bool)]) {
        let mut form = Form::new(title, kind, fields);
        if let Some(id) = form.kind.client() {
            let Some(peer) = self.peer(id) else {
                self.error = Some("目前无法确认目标用户身份".into());
                return;
            };
            form.peer = Some(peer);
        }
        self.form = Some(form);
    }

    pub fn create_channel_form(&mut self, parent: &str) {
        let title = if parent == "0" {
            "创建频道"
        } else {
            "创建子频道"
        };
        self.prompt(
            title,
            Kind::CreateChannel(parent.into()),
            &[
                ("名称", "", false),
                ("主题", "", false),
                ("描述", "", false),
                ("频道密码（可选）", "", true),
            ],
        );
    }

    pub fn edit_channel_form(&mut self, channel: &Channel) {
        let topic = channel.topic.as_deref().unwrap_or_default();
        let mut fields = vec![
            ("名称", channel.name.as_str(), false),
            ("主题", topic, false),
        ];
        if let Some(desc) = channel.description.as_deref() {
            fields.push(("描述", desc, false));
        }
        self.prompt("编辑频道", Kind::Edit(channel.id.clone()), &fields);
    }

    pub fn edit_bookmark(&mut self, index: Option<usize>, b: Bookmark) {
        let title = if index.is_some() {
            "编辑收藏"
        } else {
            "添加收藏"
        };
        let mut form = Form::new(
            title,
            Kind::Bookmark(index),
            &[
                ("名称", &b.name, false),
                ("地址", &b.address, false),
                ("昵称", &b.nickname, false),
                ("默认频道", &b.channel, false),
                ("分组", &b.group, false),
            ],
        );
        form.identity = b.identity;
        self.form = Some(form);
    }

    pub fn import_bookmarks(&mut self, input: &str) -> Result<(), String> {
        if input.len() > 1_000_000 {
            return Err("收藏文件过大".into());
        }
        let entries: Vec<Bookmark> = serde_json::from_str(input)
            .map_err(|e| format!("收藏 JSON 无效（第 {} 行）", e.line()))?;
        if entries.len() > 1_000 {
            return Err("一次最多导入 1,000 个收藏".into());
        }
        for entry in &entries {
            entry.validate()?;
        }
        let previous = self.preferences.bookmarks.len();
        for entry in entries {
            if !self.preferences.bookmarks.iter().any(|s| {
                s.address == entry.address
                    && s.channel == entry.channel
                    && s.nickname == entry.nickname
                    && s.group == entry.group
                    && s.identity == entry.identity
            }) {
                self.preferences.bookmarks.push(entry);
            }
        }
        self.notice = format!(
            "已导入 {} 个新收藏",
            self.preferences.bookmarks.len() - previous
        );
        self.backend.save_preferences(self.preferences.clone());
        Ok(())
    }

    pub fn connection_window(&mut self, ctx: &egui::Context) {
        if !self.connect_open {
            return;
        }
        let mut open = true;
        egui::Window::new("连接服务器")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(400_f32.min(ctx.content_rect().width() - 32.))
            .show(ctx, |ui| {
                egui::Grid::new("connection-fields")
                    .num_columns(2)
                    .spacing([12., 8.])
                    .show(ui, |ui| {
                        ui.label("服务器地址");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.preferences.address)
                                .hint_text("voice.example.com:9987")
                                .desired_width(280_f32.min(ctx.content_rect().width() - 152.)),
                        );
                        ui.end_row();
                        ui.label("昵称");
                        ui.text_edit_singleline(&mut self.preferences.nickname);
                        ui.end_row();
                        ui.label("连接身份");
                        ui.add_enabled_ui(!self.temporary_identity, |ui| {
                            identity_choice(
                                ui,
                                "connect-identity",
                                &mut self.preferences.identity,
                                &self.preferences.identities,
                            );
                        });
                        ui.end_row();
                        ui.label("服务器密码");
                        ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                        ui.end_row();
                        ui.label("默认频道");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.preferences.channel)
                                .hint_text("频道路径或 /频道ID"),
                        );
                        ui.end_row();
                        ui.label("频道密码");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.channel_password).password(true),
                        );
                        ui.end_row();
                    });
                ui.separator();
                ui.checkbox(
                    &mut self.temporary_identity,
                    "使用临时测试身份（不继承原身份权限，不保存）",
                );
                ui.weak("密码只用于本次连接，不写入收藏或普通配置。");
                ui.horizontal(|ui| {
                    let can_connect = self.inactive() && !self.identity_busy;
                    if ui
                        .add_enabled(can_connect, egui::Button::new("立即连接"))
                        .clicked()
                    {
                        self.connect();
                    }
                    if ui.button("收藏夹…").clicked() {
                        self.bookmarks_open = true;
                    }
                });
            });
        self.connect_open &= open;
        if !self.connect_open {
            self.password.clear();
            self.channel_password.clear();
        }
    }

    pub fn bookmarks_window(&mut self, ctx: &egui::Context) {
        if !self.bookmarks_open {
            return;
        }
        let mut open = true;
        egui::Window::new("服务器收藏")
            .open(&mut open)
            .default_width(460.)
            .vscroll(true)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("＋ 添加当前").clicked() {
                        let bookmark = Bookmark {
                            name: self.snapshot.server_name.clone().unwrap_or_default(),
                            address: self.preferences.address.clone(),
                            nickname: self.preferences.nickname.clone(),
                            channel: self.preferences.channel.clone(),
                            group: String::new(),
                            identity: self.preferences.identity,
                        };
                        self.edit_bookmark(None, bookmark);
                    }
                    if ui.button("导入 JSON…").clicked() {
                        self.prompt(
                            "导入 CongSpeak 收藏",
                            Kind::ImportBookmarks,
                            &[("收藏 JSON", "", false)],
                        );
                    }
                    if ui.button("复制 JSON").clicked() {
                        match serde_json::to_string_pretty(&self.preferences.bookmarks) {
                            Ok(json) => {
                                ctx.copy_text(json);
                                self.notice = "已复制收藏".into();
                            }
                            Err(e) => self.error = Some(e.to_string()),
                        }
                    }
                });
                ui.separator();
                if self.preferences.bookmarks.is_empty() {
                    ui.weak("暂无收藏。点击上方「＋ 添加当前」保存常用服务器。");
                }
                let mut groups = BTreeMap::<String, Vec<_>>::new();
                for (i, b) in self.preferences.bookmarks.iter().cloned().enumerate() {
                    groups.entry(b.group.clone()).or_default().push((i, b));
                }
                for (group, bookmarks) in groups {
                    let label = if group.is_empty() {
                        "未分组"
                    } else {
                        &group
                    };
                    egui::CollapsingHeader::new(label)
                        .id_salt(&group)
                        .default_open(true)
                        .show(ui, |ui| {
                            for (index, b) in bookmarks {
                                ui.push_id(index, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        if ui
                                            .add_enabled(
                                                self.inactive(),
                                                egui::Button::new(&b.name),
                                            )
                                            .on_hover_text(&b.address)
                                            .clicked()
                                        {
                                            self.preferences.address = b.address.clone();
                                            self.preferences.nickname = b.nickname.clone();
                                            self.preferences.channel = b.channel.clone();
                                            self.preferences.identity = b.identity;
                                            self.temporary_identity = false;
                                            self.connect_open = true;
                                        }
                                        ui.weak(&b.address);
                                        if ui.small_button("编辑").clicked() {
                                            self.edit_bookmark(Some(index), b.clone());
                                        }
                                        if ui.small_button("删除").clicked() {
                                            self.prompt(
                                                format!("删除收藏「{}」？", b.name),
                                                Kind::DeleteBookmark(index),
                                                &[],
                                            );
                                        }
                                    });
                                });
                            }
                        });
                }
            });
        self.bookmarks_open = open;
    }

    pub fn identities_window(&mut self, ctx: &egui::Context) {
        if !self.identities_open {
            return;
        }
        let mut open = true;
        let editable = self.inactive() && !self.identity_busy;
        egui::Window::new("本地身份")
            .open(&mut open)
            .default_width(500.)
            .vscroll(true)
            .show(ctx, |ui| {
                if self.identity_busy {
                    ui.spinner();
                } else if !editable {
                    ui.weak("断开服务器后可管理身份。");
                }
                ui.add_enabled_ui(editable, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("＋ 新建…").clicked() {
                            self.prompt("新建身份", Kind::NewIdentity, &[("名称", "", false)]);
                        }
                        if ui.button("导入 TS3 INI…").clicked() {
                            self.prompt(
                                "导入 TS3 身份",
                                Kind::ImportIdentity,
                                &[("名称", "", false), ("INI 文件路径", "", false)],
                            );
                        }
                    });
                    ui.separator();
                    let mut profiles = vec![(None, "默认身份".to_owned())];
                    profiles.extend(
                        self.preferences
                            .identities
                            .iter()
                            .map(|(id, name)| (Some(*id), name.clone())),
                    );
                    for (id, name) in profiles {
                        ui.horizontal_wrapped(|ui| {
                            ui.strong(&name);
                            if ui.small_button("详情").clicked() {
                                self.run_identity(move || {
                                    identities::info(identity_mode(id))
                                        .map(|info| IdentityChange::Info(id, info))
                                });
                            }
                            if ui.small_button("导出…").clicked() {
                                self.prompt(
                                    "导出 TS3 身份",
                                    Kind::ExportIdentity(id, name.clone()),
                                    &[("INI 保存路径", "", false)],
                                );
                            }
                            if let Some(id) = id {
                                if ui.small_button("重命名…").clicked() {
                                    self.prompt(
                                        "重命名身份",
                                        Kind::RenameIdentity(id),
                                        &[("名称", &name, false)],
                                    );
                                }
                                if ui.small_button("删除…").clicked() {
                                    self.prompt(
                                        format!("删除身份「{name}」？"),
                                        Kind::DeleteIdentity(id),
                                        &[],
                                    );
                                }
                            }
                        });
                        if let Some((selected, info)) = &self.identity_details
                            && *selected == id
                        {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(format!("安全等级 {}", info.level));
                                ui.monospace(&info.uid);
                                if ui.small_button("复制 UID").clicked() {
                                    ctx.copy_text(info.uid.clone());
                                }
                            });
                        }
                    }
                });
            });
        self.identities_open = open;
    }

    pub fn contacts_window(&mut self, ctx: &egui::Context) {
        if !self.contacts_open {
            return;
        }
        let mut open = true;
        let mut remove = None;
        egui::Window::new("联系人")
            .open(&mut open)
            .default_width(400_f32.min(ctx.content_rect().width() - 32.))
            .vscroll(true)
            .show(ctx, |ui| {
                if self.preferences.contacts.is_empty() {
                    ui.weak("右键或选择频道中的用户，通过「联系人」添加好友或忽略。");
                }
                let list: Vec<_> = self
                    .preferences
                    .contacts
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                for (uid, contact) in list {
                    ui.push_id(&uid, |ui| {
                        ui.horizontal(|ui| {
                            ui.strong(&contact.name);
                            ui.weak(if contact.ignored {
                                "已忽略"
                            } else {
                                "好友"
                            });
                            if ui.small_button("编辑").clicked() {
                                self.prompt(
                                    "编辑联系人",
                                    Kind::Contact(uid.clone(), contact.ignored),
                                    &[
                                        ("名称", &contact.name, false),
                                        ("备注", &contact.note, false),
                                    ],
                                );
                            }
                            if ui.small_button("移除").clicked() {
                                remove = Some(uid.clone());
                            }
                        });
                        if !contact.note.is_empty() {
                            ui.label(&contact.note);
                        }
                        ui.separator();
                    });
                }
            });
        self.contacts_open = open;
        if let Some(uid) = remove {
            self.preferences.contacts.remove(&uid);
            self.save_contacts();
        }
    }

    pub fn network_window(&mut self, ctx: &egui::Context) {
        if !self.network_open {
            return;
        }
        egui::Window::new("连接信息")
            .open(&mut self.network_open)
            .collapsible(false)
            .resizable(false)
            .default_width(320.)
            .show(ctx, |ui| {
                ui.label(status_name(&self.snapshot.status));
                if let Some(name) = &self.snapshot.server_name {
                    ui.strong(name);
                }
                if let Some(stats) = &self.snapshot.network {
                    ui.separator();
                    ui.label(format!(
                        "往返延迟 {:.0} ms · 波动 {:.0} ms",
                        stats.round_trip_ms, stats.variation_ms
                    ));
                    ui.label(format!(
                        "接收语音丢包 {:.2}% · 接收总丢包 {:.2}%",
                        stats.inbound_voice_loss * 100.,
                        stats.inbound_loss * 100.
                    ));
                    ui.label(format!(
                        "已接收 {} KiB · 已发送 {} KiB",
                        stats.inbound_bytes / 1024,
                        stats.outbound_bytes / 1024
                    ));
                    ctx.request_repaint_after(Duration::from_secs(1));
                }
            });
    }

    pub fn form_window(&mut self, ctx: &egui::Context) {
        let Some(mut form) = self.form.take() else {
            return;
        };
        let mut open = true;
        let mut submit = false;
        egui::Window::new(&form.title)
            .id(egui::Id::unique("action-form"))
            .open(&mut open)
            .collapsible(false)
            .default_width(360.)
            .show(ctx, |ui| {
                if matches!(form.kind, Kind::CreateChannel(_)) {
                    egui::ComboBox::from_label("频道类型")
                        .selected_text(channel_lifetime_name(form.lifetime))
                        .show_ui(ui, |ui| {
                            for lt in [
                                ChannelLifetime::Temporary,
                                ChannelLifetime::SemiPermanent,
                                ChannelLifetime::Permanent,
                            ] {
                                ui.selectable_value(
                                    &mut form.lifetime,
                                    lt,
                                    channel_lifetime_name(lt),
                                );
                            }
                        });
                }
                if matches!(form.kind, Kind::DeleteChannel(_)) {
                    ui.colored_label(Color32::YELLOW, "频道及其资料将从服务器删除。");
                }
                if let Kind::MoveChannel(id) = &form.kind {
                    let (parent, order) = form.fields.split_at_mut(1);
                    let (parent, order) = (&mut parent[0].1, &mut order[0].1);
                    let prev = parent.clone();
                    let roots = self.snapshot.channels.iter().filter(|c| &c.id != id);
                    channel_choice(ui, "目标上级", parent, "服务器根目录", roots);
                    if prev != *parent {
                        *order = "0".into();
                    }
                    let siblings = self
                        .snapshot
                        .channels
                        .iter()
                        .filter(|c| &c.id != id && &c.parent_id == parent);
                    channel_choice(ui, "排在其后", order, "同级第一位", siblings);
                } else {
                    for (label, value, secret) in &mut form.fields {
                        ui.label(label.as_str());
                        if matches!(label.as_str(), "描述" | "收藏 JSON" | "备注") {
                            ui.add(
                                egui::TextEdit::multiline(value)
                                    .desired_rows(5)
                                    .desired_width(f32::INFINITY),
                            );
                        } else {
                            ui.add(
                                egui::TextEdit::singleline(value)
                                    .password(*secret)
                                    .desired_width(f32::INFINITY),
                            );
                        }
                    }
                }
                if matches!(form.kind, Kind::Kick(..) | Kind::Ban(_)) {
                    ui.colored_label(Color32::YELLOW, "此操作会影响选定用户，请确认目标与原因。");
                }
                if matches!(form.kind, Kind::Bookmark(_)) {
                    ui.label("连接身份");
                    identity_choice(
                        ui,
                        "bookmark-identity",
                        &mut form.identity,
                        &self.preferences.identities,
                    );
                }
                if matches!(form.kind, Kind::ExportIdentity(..)) {
                    ui.colored_label(Color32::YELLOW, "INI 含私钥，请仅保存到自己能访问的位置。");
                }
                if matches!(form.kind, Kind::DeleteIdentity(_)) {
                    ui.colored_label(
                        Color32::YELLOW,
                        "此操作会删除系统凭据中的私钥，请确认已备份。",
                    );
                }
                if ui.button("确定").clicked() {
                    submit = true;
                }
            });
        if submit {
            self.submit_form(form);
        } else if open {
            self.form = Some(form);
        }
    }

    fn submit_form(&mut self, form: Form) {
        if let Some(peer) = &form.peer
            && self.peer(peer.0).as_ref() != Some(peer)
        {
            self.error = Some("目标用户已离开或身份已变化，请重新选择".into());
            return;
        }
        let v: Vec<_> = form.fields.iter().map(|(_, v, _)| v.clone()).collect();
        let optional = |i: usize| v.get(i).cloned().filter(|s| !s.is_empty());
        match form.kind {
            Kind::Join(channel_id) => self.action(
                "加入频道",
                ClientAction::Join {
                    channel_id,
                    password: optional(0),
                },
            ),
            Kind::Rename => self.action("修改昵称", ClientAction::Rename { name: v[0].clone() }),
            Kind::Away => {
                let msg = if v[0].is_empty() {
                    "离开".into()
                } else {
                    v[0].clone()
                };
                self.action("设置离开", ClientAction::Away { message: Some(msg) });
            }
            Kind::Poke(client_id) => self.action(
                "Poke",
                ClientAction::Poke {
                    client_id,
                    message: v[0].clone(),
                },
            ),
            Kind::Move(client_id) => self.action(
                "移动用户",
                ClientAction::Move {
                    client_id,
                    channel_id: v[0].clone(),
                    password: optional(1),
                },
            ),
            Kind::Kick(client_id, from_server) => self.action(
                "踢出用户",
                ClientAction::Kick {
                    client_id,
                    from_server,
                    reason: v[0].clone(),
                },
            ),
            Kind::Edit(channel_id) => self.action(
                "编辑频道",
                ClientAction::EditChannel {
                    channel_id,
                    name: v[0].clone(),
                    topic: v[1].clone(),
                    description: v.get(2).cloned(),
                },
            ),
            Kind::CreateChannel(parent_id) => self.action(
                "创建频道",
                ClientAction::CreateChannel {
                    parent_id,
                    name: v[0].clone(),
                    topic: v[1].clone(),
                    description: v[2].clone(),
                    password: optional(3),
                    lifetime: form.lifetime,
                },
            ),
            Kind::MoveChannel(channel_id) => self.action(
                "移动频道",
                ClientAction::MoveChannel {
                    channel_id,
                    parent_id: v[0].clone(),
                    previous_id: Some(v[1].clone()),
                },
            ),
            Kind::DeleteChannel(channel_id) => {
                self.action("删除频道", ClientAction::DeleteChannel { channel_id })
            }
            Kind::ChannelPassword(channel_id) => self.action(
                "修改频道密码",
                ClientAction::ChannelPassword {
                    channel_id,
                    password: optional(0),
                },
            ),
            Kind::ClientDescription => self.action(
                "修改个人描述",
                ClientAction::ClientDescription {
                    description: v[0].clone(),
                },
            ),
            Kind::RequestTalk => self.action(
                "请求发言权",
                ClientAction::RequestTalk {
                    message: Some(v[0].clone()),
                },
            ),
            Kind::Ban(client_id) => match v[0].parse::<u32>() {
                Ok(minutes) if minutes > 0 => self.action(
                    "封禁用户",
                    ClientAction::BanClient {
                        client_id,
                        minutes,
                        reason: v[1].clone(),
                    },
                ),
                _ => self.error = Some("请输入大于零的封禁分钟数".into()),
            },
            Kind::Complaint(client_id, uid) => self.action(
                "提交投诉",
                ClientAction::Complaint {
                    client_id,
                    uid,
                    message: v[0].clone(),
                },
            ),
            Kind::RemoveBan { generation, ban_id } => {
                let id = self.id();
                if let Some(window) = &mut self.ban_window
                    && window.generation == generation
                    && window.removing.is_none()
                {
                    window.removing = Some((id, ban_id));
                    self.submit_action(
                        id,
                        "解除封禁",
                        ClientAction::RemoveBan { generation, ban_id },
                    );
                }
            }
            Kind::Contact(uid, ignored) => {
                let name = v[0].trim();
                if name.is_empty() || name.len() > 256 || v[1].len() > 4096 {
                    self.error = Some("联系人名称或备注不符合长度要求".into());
                } else {
                    self.preferences.contacts.insert(
                        uid,
                        Contact {
                            name: name.into(),
                            note: v[1].clone(),
                            ignored,
                        },
                    );
                    self.save_contacts();
                }
            }
            Kind::Redeem => self.action(
                "使用权限密钥",
                ClientAction::RedeemKey { key: v[0].clone() },
            ),
            Kind::Bookmark(index) => {
                let bookmark = Bookmark {
                    name: v[0].trim().into(),
                    address: v[1].trim().into(),
                    nickname: v[2].trim().into(),
                    channel: v[3].clone(),
                    group: v[4].trim().into(),
                    identity: form.identity,
                };
                if let Err(error) = bookmark.validate() {
                    self.error = Some(error);
                } else {
                    if let Some(slot) = index.and_then(|i| self.preferences.bookmarks.get_mut(i)) {
                        *slot = bookmark;
                    } else {
                        self.preferences.bookmarks.push(bookmark);
                    }
                    self.backend.save_preferences(self.preferences.clone());
                }
            }
            Kind::ImportBookmarks => {
                if let Err(error) = self.import_bookmarks(&v[0]) {
                    self.error = Some(error);
                }
            }
            Kind::DeleteBookmark(i) => {
                if i < self.preferences.bookmarks.len() {
                    self.preferences.bookmarks.remove(i);
                    self.backend.save_preferences(self.preferences.clone());
                }
            }
            Kind::NewIdentity | Kind::RenameIdentity(_) | Kind::ImportIdentity => {
                let name = v[0].trim().to_owned();
                if name.is_empty() || name.len() > 120 || name.chars().any(char::is_control) {
                    self.error = Some("请输入有效的身份名称（最多 120 字节）".into());
                } else if let Kind::RenameIdentity(id) = form.kind {
                    if let Some(value) = self.preferences.identities.get_mut(&id) {
                        *value = name;
                        self.backend.save_preferences(self.preferences.clone());
                    }
                } else if matches!(form.kind, Kind::ImportIdentity) {
                    let path = PathBuf::from(v[1].trim());
                    self.run_identity(move || {
                        identities::import_ini(&path).map(|id| IdentityChange::Stored(id, name))
                    });
                } else {
                    self.run_identity(move || {
                        identities::create().map(|id| IdentityChange::Stored(id, name))
                    });
                }
            }
            Kind::ExportIdentity(id, name) => {
                let path = PathBuf::from(v[0].trim());
                self.run_identity(move || {
                    identities::export_ini(identity_mode(id), &path, &name)?;
                    Ok(IdentityChange::Exported)
                });
            }
            Kind::DeleteIdentity(id) => self.run_identity(move || {
                identities::remove(id)?;
                Ok(IdentityChange::Removed(id))
            }),
        }
    }
}

pub fn identity_mode(id: Option<Uuid>) -> IdentityMode {
    id.map_or(IdentityMode::SystemCredential, IdentityMode::Named)
}

pub fn identity_choice(
    ui: &mut egui::Ui,
    widget_id: &str,
    selected: &mut Option<Uuid>,
    identities: &BTreeMap<Uuid, String>,
) {
    let label = match *selected {
        None => "默认身份",
        Some(id) => identities.get(&id).map_or("身份不可用", String::as_str),
    };
    egui::ComboBox::from_id_salt(widget_id)
        .selected_text(label)
        .show_ui(ui, |ui| {
            ui.selectable_value(selected, None, "默认身份");
            for (id, name) in identities {
                ui.selectable_value(selected, Some(*id), name);
            }
        });
}

fn channel_lifetime_name(lifetime: ChannelLifetime) -> &'static str {
    match lifetime {
        ChannelLifetime::Temporary => "临时频道",
        ChannelLifetime::SemiPermanent => "半永久频道",
        ChannelLifetime::Permanent => "永久频道",
    }
}

fn channel_choice<'a>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    root: &str,
    channels: impl Iterator<Item = &'a Channel>,
) {
    let channels: Vec<_> = channels.collect();
    let name = channels
        .iter()
        .find(|c| c.id == *value)
        .map(|c| c.name.as_str())
        .unwrap_or(root);
    egui::ComboBox::from_label(label)
        .selected_text(name)
        .show_ui(ui, |ui| {
            ui.selectable_value(value, "0".into(), root);
            for c in channels {
                ui.selectable_value(value, c.id.clone(), format!("{}（{}）", c.name, c.id));
            }
        });
}
