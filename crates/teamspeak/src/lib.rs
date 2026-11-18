pub mod files;
pub mod identities;
pub mod moderation;

use congspeak_audio::runtime::{AudioHandle, Control, EncodedAudio};
use congspeak_core::{AudioSettings, TalkMode};
use congspeak_core::{ChatTarget, ClientAction};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};
use tsclientlib::{data::exts::M2BClientUpdateExt, messages::OutMessageTrait};
use tsproto_packets::packets::{AudioData, CodecType, OutAudio};

use congspeak_core::{Channel, Client, ConnectRequest, ConnectionStatus, SessionEvent, Snapshot};
use futures::StreamExt;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tsclientlib::{
    ConnectOptions, Connection, DisconnectOptions, Identity, MessageTarget, OutCommandExt,
    StreamItem,
};

type Reply = oneshot::Sender<Result<(), String>>;
enum Command {
    Connect(ConnectRequest, Option<IdentityMode>, Reply),
    Disconnect(Reply),
    Request(RequestTarget, Operation, Reply),
    UserVolume(u32, u16, Option<String>, f32, Reply),
    AudioSettings(AudioSettings, Reply),
    Ptt(bool, Reply),
    MicTest(bool, Reply),
    TestSpeaker(Reply),
    PresenceTone(u32, bool),
    RestartAudio(Reply),
    Files(files::Command),
    Bans(u32, moderation::BanReply),
    IgnoreUids(HashSet<String>, Reply),
}

enum RequestTarget {
    ReportEvent(u32),
    Confirm {
        generation: u32,
        peer: Option<(u16, String)>,
    },
}

enum Operation {
    Message(ChatTarget, String),
    Action(ClientAction),
}

enum Confirmation {
    Event(u32),
    Direct(Reply),
}

impl Confirmation {
    fn finish(self, events: &broadcast::Sender<SessionEvent>, result: Result<(), String>) {
        match self {
            Self::Event(request_id) => {
                let _ = events.send(SessionEvent::CommandResult {
                    request_id,
                    error: result.err(),
                });
            }
            Self::Direct(reply) => {
                let _ = reply.send(result);
            }
        }
    }
}

#[derive(Clone, Copy)]
pub enum IdentityMode {
    SystemCredential,
    Named(uuid::Uuid),
    Ephemeral,
    #[cfg(test)]
    DelayedTest,
}

/// Owns no protocol state: all operations are serialized by the session actor.
#[derive(Clone)]
pub struct Session {
    #[cfg(test)]
    transport_fault: mpsc::Sender<tsclientlib::Error>,
    commands: mpsc::Sender<Command>,
    state: watch::Receiver<Snapshot>,
    events: broadcast::Sender<SessionEvent>,
    settings: Arc<Mutex<AudioSettings>>,
    hotkey: watch::Sender<bool>,
}

impl Session {
    pub fn start(identity_mode: IdentityMode) -> Self {
        Self::start_inner(identity_mode, AudioSettings::default(), false)
    }
    pub fn start_with_audio(identity_mode: IdentityMode, settings: AudioSettings) -> Self {
        Self::start_inner(identity_mode, settings, true)
    }
    fn start_inner(
        identity_mode: IdentityMode,
        settings: AudioSettings,
        audio_enabled: bool,
    ) -> Self {
        let (commands, receiver) = mpsc::channel(32);
        #[cfg(test)]
        let (transport_fault, faults) = mpsc::channel(1);
        let (state_tx, state) = watch::channel(Snapshot::default());
        let (events, _) = broadcast::channel(256);
        let (hotkey, hotkey_state) = watch::channel(false);
        let stored = Arc::new(Mutex::new(settings.clone()));
        let (audio, outgoing) = AudioHandle::spawn(settings.clone());
        tokio::spawn(run(
            receiver,
            #[cfg(test)]
            faults,
            hotkey_state,
            state_tx,
            events.clone(),
            identity_mode,
            AudioSession {
                handle: audio,
                outgoing,
                settings,
                stored: stored.clone(),
                enabled: audio_enabled,
                epoch: 0,
                ptt: false,
                mic_test: false,
                permissions: (false, false),
                last_flags: None,
                channel: None,
                sent_packets: 0,
                last_sent: None,
                last_permissions: None,
                send_error: None,
            },
        ));
        Self {
            #[cfg(test)]
            transport_fault,
            commands,
            state,
            events,
            settings: stored,
            hotkey,
        }
    }
    pub fn audio_settings(&self) -> AudioSettings {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    async fn audio_command(&self, command: impl FnOnce(Reply) -> Command) -> Result<(), String> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(command(reply))
            .await
            .map_err(|_| "会话任务已退出")?;
        result.await.map_err(|_| "会话任务已退出")?
    }
    pub async fn set_audio_settings(&self, settings: AudioSettings) -> Result<(), String> {
        settings.validate()?;
        self.audio_command(|reply| Command::AudioSettings(settings, reply))
            .await
    }
    pub fn native_ptt(&self, held: bool) {
        self.hotkey.send_replace(held);
    }
    pub async fn set_ptt(&self, held: bool) -> Result<(), String> {
        self.audio_command(|reply| Command::Ptt(held, reply)).await
    }
    pub async fn mic_test(&self, enabled: bool) -> Result<(), String> {
        self.audio_command(|reply| Command::MicTest(enabled, reply))
            .await
    }
    pub async fn presence_tone(&self, generation: u32, joined: bool) -> Result<(), String> {
        self.commands
            .send(Command::PresenceTone(generation, joined))
            .await
            .map_err(|_| "会话任务已退出".into())
    }
    pub async fn test_speaker(&self) -> Result<(), String> {
        self.audio_command(Command::TestSpeaker).await
    }
    pub async fn restart_audio(&self) -> Result<(), String> {
        self.audio_command(Command::RestartAudio).await
    }

    pub fn snapshot(&self) -> Snapshot {
        self.state.borrow().clone()
    }
    pub fn watch(&self) -> watch::Receiver<Snapshot> {
        self.state.clone()
    }
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.events.subscribe()
    }

    pub async fn connect(&self, request: ConnectRequest) -> Result<(), String> {
        self.connect_mode(request, None).await
    }
    /// Explicit temporary identity for diagnostics; never overwrites the stored identity.
    pub async fn connect_temporary(&self, request: ConnectRequest) -> Result<(), String> {
        self.connect_mode(request, Some(IdentityMode::Ephemeral))
            .await
    }
    pub async fn connect_named(
        &self,
        request: ConnectRequest,
        id: uuid::Uuid,
    ) -> Result<(), String> {
        self.connect_mode(request, Some(IdentityMode::Named(id)))
            .await
    }
    async fn connect_mode(
        &self,
        request: ConnectRequest,
        mode: Option<IdentityMode>,
    ) -> Result<(), String> {
        request.validate()?;
        let (reply, result) = oneshot::channel();
        self.commands
            .send(Command::Connect(request, mode, reply))
            .await
            .map_err(|_| "会话任务已退出")?;
        result.await.map_err(|_| "会话任务已退出")?
    }

    pub async fn disconnect(&self) -> Result<(), String> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(Command::Disconnect(reply))
            .await
            .map_err(|_| "会话任务已退出")?;
        result.await.map_err(|_| "会话任务已退出")?
    }

    pub async fn action(&self, request_id: u32, action: ClientAction) -> Result<(), String> {
        self.request(
            RequestTarget::ReportEvent(request_id),
            Operation::Action(action),
        )
        .await
    }
    pub async fn action_confirmed(
        &self,
        generation: u32,
        peer: Option<(u16, String)>,
        action: ClientAction,
    ) -> Result<(), String> {
        self.request(
            RequestTarget::Confirm { generation, peer },
            Operation::Action(action),
        )
        .await
    }
    pub async fn ignore_uids(&self, uids: HashSet<String>) -> Result<(), String> {
        self.audio_command(|reply| Command::IgnoreUids(uids, reply))
            .await
    }

    pub async fn user_volume(&self, client_id: u16, volume: f32) -> Result<(), String> {
        let generation = self.state.borrow().generation;
        self.set_user_volume(generation, client_id, None, volume)
            .await
    }
    pub async fn set_user_volume(
        &self,
        generation: u32,
        client_id: u16,
        uid: Option<String>,
        volume: f32,
    ) -> Result<(), String> {
        if !volume.is_finite() || !(0.0..=2.0).contains(&volume) {
            return Err("个人音量超出范围".into());
        }
        self.audio_command(|r| Command::UserVolume(generation, client_id, uid, volume, r))
            .await
    }
    pub async fn chat(
        &self,
        request_id: u32,
        target: ChatTarget,
        text: String,
    ) -> Result<(), String> {
        self.request(
            RequestTarget::ReportEvent(request_id),
            Operation::Message(target, text),
        )
        .await
    }
    pub async fn chat_confirmed(
        &self,
        generation: u32,
        peer: Option<(u16, String)>,
        target: ChatTarget,
        text: String,
    ) -> Result<(), String> {
        self.request(
            RequestTarget::Confirm { generation, peer },
            Operation::Message(target, text),
        )
        .await
    }
    pub async fn send_message(&self, request_id: u32, text: String) -> Result<(), String> {
        self.chat(request_id, ChatTarget::Channel, text).await
    }
    async fn request(&self, target: RequestTarget, operation: Operation) -> Result<(), String> {
        if let Operation::Message(_, text) = &operation
            && (text.trim().is_empty() || text.len() > 1024 || text.contains('\0'))
        {
            return Err("消息不能为空、包含空字符或超过 1024 字节".into());
        }
        self.audio_command(|reply| Command::Request(target, operation, reply))
            .await
    }
}

fn publish(
    state: &mut Snapshot,
    tx: &watch::Sender<Snapshot>,
    events: &broadcast::Sender<SessionEvent>,
) {
    state.sequence += 1;
    tx.send_replace(state.clone());
    let _ = events.send(SessionEvent::Snapshot {
        snapshot: Box::new(state.clone()),
    });
}

fn synchronize(con: &Connection, state: &mut Snapshot, channel_list_complete: bool) {
    if let Ok(book) = con.get_state() {
        state.server_name = Some(book.server.name.clone());
        state.own_client_id = Some(book.own_client.0);
        state.channels = book
            .channels
            .values()
            .map(|channel| Channel {
                id: channel.id.0.to_string(),
                parent_id: channel.parent.0.to_string(),
                previous_id: channel.order.0.to_string(),
                name: channel.name.clone(),
                codec: format!("{:?}", channel.codec),
                topic: channel.topic.clone(),
                description: channel
                    .optional_data
                    .as_ref()
                    .map(|data| data.description.clone()),
                subscribed: channel.subscribed,
                has_password: channel.has_password.unwrap_or(false),
            })
            .collect();
        state.channels.sort_by(|a, b| a.id.cmp(&b.id));
        state.clients = book
            .clients
            .values()
            .map(|client| Client {
                id: client.id.0,
                channel_id: client.channel.0.to_string(),
                name: client.name.clone(),
                uid: client.uid.as_ref().map(|v| v.to_string()),
                input_muted: client.input_muted,
                output_muted: client.output_muted,
                away: client.away_message.clone(),
                description: client.description.clone(),
            })
            .collect();
        state.clients.sort_by_key(|client| client.id);
        let ready = channel_list_complete
            && book
                .clients
                .get(&book.own_client)
                .is_some_and(|client| book.channels.contains_key(&client.channel));
        state.transition(if ready {
            ConnectionStatus::Connected
        } else {
            ConnectionStatus::Synchronizing
        });
    }
}

// Never poll a connection after a terminal error (upstream issue #43).
async fn next_event(
    con: &mut Option<Connection>,
    #[cfg(test)] faults: &mut mpsc::Receiver<tsclientlib::Error>,
) -> Option<Result<StreamItem, tsclientlib::Error>> {
    match con {
        Some(con) => {
            let mut events = con.events();
            #[cfg(test)]
            {
                tokio::select! {
                    Some(fault) = faults.recv() => Some(Err(fault)),
                    event = events.next() => event,
                }
            }
            #[cfg(not(test))]
            events.next().await
        }
        None => std::future::pending().await,
    }
}

fn receive_queue_overflow(error: &tsclientlib::Error) -> bool {
    matches!(
        error,
        tsclientlib::Error::ConnectionFailed(tsproto::client::Error::TsProto(
            tsproto::Error::MaxLengthExceeded("command queue" | "fragment queue")
        ))
    )
}

fn recovery_options(connection: &Connection) -> Option<ConnectOptions> {
    let book = connection.get_state().ok()?;
    let server = tsproto_types::UidBuf(book.server.public_key.get_uid_no_base64());
    // The failed transport can't safely resume processing ordered commands.
    // Keep the configured address and local identity, and verify the previous server's key.
    Some(connection.get_options().clone().server(server))
}

struct QueueRecovery {
    options: ConnectOptions,
    retry_at: Option<tokio::time::Instant>,
}

const QUEUE_RECOVERY_TIMEOUT: Duration = Duration::from_secs(45);

async fn close(con: &mut Option<Connection>) {
    if let Some(mut active) = con.take() {
        // Upstream disconnect() is a no-op during setup. Drop the attempt instead of continuing it.
        // A remote slot already allocated during setup may still remain until server timeout.
        if active.get_state().is_ok() && active.disconnect(DisconnectOptions::new()).is_ok() {
            let _ = tokio::time::timeout(Duration::from_secs(2), async {
                while let Some(event) = active.events().next().await {
                    if event.is_err() {
                        break;
                    }
                }
            })
            .await;
        }
    }
}

fn fail_pending(
    pending: &mut HashMap<u16, (Confirmation, tokio::time::Instant)>,
    events: &broadcast::Sender<SessionEvent>,
) {
    for (_, (confirmation, _)) in pending.drain() {
        confirmation.finish(events, Err("连接已中断，发送结果未知".into()));
    }
}

async fn run(
    mut commands: mpsc::Receiver<Command>,
    #[cfg(test)] mut faults: mpsc::Receiver<tsclientlib::Error>,
    mut hotkey: watch::Receiver<bool>,
    tx: watch::Sender<Snapshot>,
    events: broadcast::Sender<SessionEvent>,
    identity_mode: IdentityMode,
    mut audio: AudioSession,
) {
    let mut state = Snapshot::default();
    let mut connection = None;
    let mut preparing: Option<PendingIdentity> = None;
    let identity_busy = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut active_identity_mode = identity_mode;
    let mut pending = HashMap::new();
    let mut files = files::Requests::default();
    let mut bans = moderation::BanRequests::default();
    let mut ignored_uids: HashSet<String> = HashSet::new();
    let mut ignored_ids = HashSet::new();
    let mut attempt_started = None;
    let mut last_queue_recovery: Option<tokio::time::Instant> = None;
    let mut queue_recovery: Option<QueueRecovery> = None;
    let mut channel_list_complete = false;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut keyboard_ptt = false;
    let mut button_ptt = false;
    let mut saving_identity = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            Ok(()) = hotkey.changed() => {
                keyboard_ptt = *hotkey.borrow_and_update() && state.status == ConnectionStatus::Connected;
                audio.set_ptt(&mut connection, keyboard_ptt || button_ptt);
            }
            Some(result) = saving_identity.join_next() => {
                if !matches!(result, Ok(Ok(()))) {
                    state.last_error = Some("身份等级更新未能保存到系统凭据".into());
                    publish(&mut state, &tx, &events);
                }
            }
            command = commands.recv() => {
                match command {
                    Some(Command::Connect(request, mode, reply)) => {
                        if connection.is_some() || preparing.is_some() || queue_recovery.is_some() {
                            let _=reply.send(Err("请先断开当前连接".into()));continue;
                        }
                        let mode=mode.unwrap_or(identity_mode);
                        let persistent=!matches!(mode,IdentityMode::Ephemeral);
                        if persistent && !saving_identity.is_empty() {
                            let _=reply.send(Err("正在保存身份安全等级，请稍后重试".into()));
                            continue;
                        }
                        if persistent && identity_busy.swap(true,std::sync::atomic::Ordering::SeqCst) {
                            let _=reply.send(Err("系统身份读取仍未返回，请检查系统凭据授权；可显式选择临时身份进行诊断".into()));continue;
                        }
                        last_queue_recovery = None;
                        channel_list_complete=false;state.generation+=1;state.sequence=0;state.clear_server();state.last_error=None;
                        state.notice=Some("正在读取本地身份…".into());state.transition(ConnectionStatus::Connecting);publish(&mut state,&tx,&events);
                        let busy=identity_busy.clone();
                        let task=tokio::task::spawn_blocking(move || {
                            let result=std::panic::catch_unwind(||identities::load(mode)).unwrap_or_else(|_|Err("身份存储任务异常".into()));
                            if persistent {busy.store(false,std::sync::atomic::Ordering::SeqCst);}
                            result
                        });
                        preparing=Some(PendingIdentity{task,request,reply,mode,started:tokio::time::Instant::now()});
                    }
                    Some(Command::Disconnect(reply)) => {
                        if let Some(preparation)=preparing.take(){let _=preparation.reply.send(Err("连接已取消".into()));}
                        state.transition(ConnectionStatus::Disconnecting); publish(&mut state, &tx, &events);
                        keyboard_ptt = false; button_ptt = false; hotkey.borrow_and_update();
                        audio.stop();
                        queue_recovery = None;
                        close(&mut connection).await;
                        files.disconnected();
                        bans.disconnected();
                        ignored_ids.clear();
                        fail_pending(&mut pending, &events);
                        attempt_started = None;
                        state.clear_server(); state.notice = None;
                        state.transition(ConnectionStatus::Disconnected); publish(&mut state, &tx, &events);
                        let _ = reply.send(Ok(()));
                    }
                    Some(Command::AudioSettings(settings,reply)) => {
                        if audio.settings.changes_stream_processing(&settings) || settings.ptt_key != audio.settings.ptt_key {
                            audio.end(&mut connection);
                            keyboard_ptt = false; button_ptt = false; hotkey.borrow_and_update();
                            audio.epoch+=1; audio.ptt=false;audio.send_error=None;
                        }
                        let result=audio.handle.control(Control::Configure(settings.clone(),audio.epoch));
                        if result.is_ok(){audio.settings=settings;*audio.stored.lock().unwrap_or_else(|e|e.into_inner())=audio.settings.clone();audio.sync_flags(&mut connection);audio.permissions(&connection);}
                        let _=reply.send(result);
                    }
                    Some(Command::Ptt(held, reply)) => {
                        button_ptt = held && state.status == ConnectionStatus::Connected;
                        audio.set_ptt(&mut connection, keyboard_ptt || button_ptt);
                        let _ = reply.send(Ok(()));
                    }
                    Some(Command::MicTest(enabled,reply)) => {
                        keyboard_ptt = false; button_ptt = false; hotkey.borrow_and_update();
                        audio.epoch+=1;audio.ptt=false;audio.mic_test=enabled;audio.end(&mut connection);audio.last_flags=None;
                        let result = audio.handle.control(Control::MicTest(enabled,audio.epoch));
                        audio.sync_flags(&mut connection);audio.permissions(&connection);
                        let _=reply.send(result);
                    }
                    Some(Command::PresenceTone(generation, joined)) => {
                        if generation == state.generation && state.status == ConnectionStatus::Connected {
                            let _ = audio.handle.control(Control::PresenceTone(joined));
                        }
                    }
                    Some(Command::TestSpeaker(reply)) => {let _=reply.send(audio.handle.control(Control::TestSpeaker));}
                    Some(Command::RestartAudio(reply)) => {
                        audio.end(&mut connection);audio.epoch+=1;audio.last_flags=None;
                        let _=reply.send(audio.handle.control(Control::Restart(audio.epoch)));
                    }
                    Some(Command::Request(target, operation, reply)) => {
                        if pending.len() >= 256 {
                            let _ = reply.send(Err("等待确认的请求过多，请稍后重试".into()));
                            continue;
                        }
                        let result = connection.as_mut().ok_or_else(|| "尚未连接服务器".to_owned())
                            .and_then(|con| submit(con, &state, &target, &operation));
                        match result {
                            Ok(handle) => {
                                let confirmation = match target {
                                    RequestTarget::ReportEvent(id) => {
                                        let _ = reply.send(Ok(()));
                                        Confirmation::Event(id)
                                    }
                                    RequestTarget::Confirm { .. } => Confirmation::Direct(reply),
                                };
                                pending.insert(handle.0, (confirmation, tokio::time::Instant::now()));
                            }
                            Err(error) => { let _ = reply.send(Err(error)); }
                        }
                    }
                    Some(Command::Files(request)) => files.request(connection.as_mut().filter(|_| state.status == ConnectionStatus::Connected), state.generation, request),
                    Some(Command::Bans(generation, reply)) => bans.request(connection.as_mut().filter(|_| state.status == ConnectionStatus::Connected && generation == state.generation), reply),
                    Some(Command::IgnoreUids(uids,reply))=>{
                        ignored_uids = uids;
                        sync_ignored(&state, &ignored_uids, &mut ignored_ids, &audio.handle);
                        let _ = reply.send(Ok(()));
                    }
                    Some(Command::UserVolume(generation, id, uid, volume, reply)) => {
                        let result = if generation == state.generation && state.status == ConnectionStatus::Connected
                            && state.clients.iter().any(|c| c.id == id && uid.as_ref().is_none_or(|uid| c.uid.as_ref() == Some(uid)))
                        {
                            audio.handle.control(Control::UserVolume(id, volume))
                        } else {
                            Err("用户已离开或连接已变化".into())
                        };
                        let _ = reply.send(result);
                    }
                    None => { audio.stop(); fail_pending(&mut pending, &events); close(&mut connection).await; break; }
                }
            }
            result = next_identity(&mut preparing) => {
                let Some(preparation)=preparing.take() else {continue;};
                let request=preparation.request;
                let result=result.and_then(|identity|{
                    let mut options=Connection::build(request.address.trim().to_owned()).name(request.nickname.trim().to_owned()).identity(identity)
                        .input_muted(!audio.enabled||audio.settings.input_muted).output_muted(!audio.enabled||audio.settings.output_muted)
                        .input_hardware_enabled(false).output_hardware_enabled(false);
                    if let Some(password)=request.password {options=options.password(password);}
                    if let Some(channel)=request.channel.filter(|s|!s.is_empty()){options=options.channel(channel);}
                    if let Some(password)=request.channel_password{options=options.channel_password(password);}
                    options.connect().map_err(|e|e.to_string())
                });
                state.notice=None;
                match result {
                    Ok(con)=>{active_identity_mode=preparation.mode;if audio.enabled{let _=audio.handle.control(Control::Start);}connection=Some(con);attempt_started=Some(tokio::time::Instant::now());let _=preparation.reply.send(Ok(()));},
                    Err(error)=>{state.last_error=Some(error.clone());state.transition(ConnectionStatus::Failed);let _=preparation.reply.send(Err(error));}
                }
                publish(&mut state,&tx,&events);
            }
            frame = next_encoded(&mut audio.outgoing) => {
                if state.status == ConnectionStatus::Connected && frame.epoch==audio.epoch && frame.created.elapsed()<Duration::from_millis(100)
                    && audio.enabled && audio.permissions.0 && !audio.mic_test
                    && !audio.settings.input_muted && !audio.settings.output_muted
                    && (audio.settings.talk_mode!=TalkMode::PushToTalk || button_ptt || (keyboard_ptt && *hotkey.borrow()) || frame.bytes.is_empty())
                    && let Some(con)=connection.as_mut().filter(|con| con.can_send_audio()) {
                    let packet=OutAudio::new(&AudioData::C2S{id:0,codec:if frame.music{CodecType::OpusMusic}else{CodecType::OpusVoice},data:&frame.bytes});
                    match con.send_audio(packet) {
                        Ok(()) if !frame.bytes.is_empty() => {audio.sent_packets+=1;audio.last_sent=Some(std::time::Instant::now());audio.send_error=None;}
                        Ok(()) => {},
                        Err(error) => {audio.send_error=Some(format!("语音发送失败：{error}"));}
                    }
                }
            }
            event = next_event(&mut connection, #[cfg(test)] &mut faults) => {
                let reopen = match &event {
                    Some(Err(error)) if receive_queue_overflow(error)
                        && last_queue_recovery.is_none_or(|time| time.elapsed() >= Duration::from_secs(60)) =>
                        connection.as_ref().and_then(recovery_options),
                    _ => None,
                };
                if reopen.is_some() || matches!(event, Some(Ok(StreamItem::DisconnectedTemporarily(_)))) {
                    channel_list_complete = false;
                    // The next TS3 connection can reuse client IDs and return codes.
                    state.generation = state.generation.wrapping_add(1);
                    state.sequence = 0;
                    state.last_error = None;
                    state.notice = Some(if reopen.is_some() {
                        "接收队列达到上限，正在恢复语音连接…"
                    } else {
                        "连接中断，正在重新连接…"
                    }.into());
                    attempt_started = Some(tokio::time::Instant::now());
                    keyboard_ptt = false;
                    button_ptt = false;
                    hotkey.borrow_and_update();
                    audio.suspend_transport();
                    ignored_ids.clear();
                    files.disconnected();
                    bans.disconnected();
                    fail_pending(&mut pending, &events);
                    state.clear_server();
                    state.transition(ConnectionStatus::Reconnecting);
                    publish(&mut state, &tx, &events);
                    if let Some(options) = reopen {
                        last_queue_recovery = attempt_started;
                        // Best-effort graceful exit on an established transport only.
                        // The server may otherwise still count it against the identity's session cap.
                        close(&mut connection).await;
                        queue_recovery = Some(QueueRecovery { options, retry_at: Some(tokio::time::Instant::now()) });
                    }
                    continue;
                }
                if matches!(event, Some(Err(tsclientlib::Error::ConnectTs(
                    tsclientlib::TsError::ClientTooManyClonesConnected | tsclientlib::TsError::ClientNicknameInuse
                )))) && let Some(recovery) = &mut queue_recovery
                {
                    connection = None;
                    recovery.retry_at = Some(tokio::time::Instant::now() + Duration::from_secs(4));
                    continue;
                }
                match event {
                    Some(Ok(StreamItem::Audio(packet))) => { if audio.enabled && state.status == ConnectionStatus::Connected {audio.handle.receive(packet);} continue; }
                    Some(Ok(StreamItem::AudioChange(change))) => {
                        match change {
                            tsclientlib::AudioEvent::CanSendAudio(value)=>audio.permissions.0=value,
                            tsclientlib::AudioEvent::CanReceiveAudio(value)=>audio.permissions.1=value,
                        }
                        audio.permissions(&connection);
                    }
                    Some(Ok(StreamItem::BookEvents(book_events))) => {
                        for event in book_events {
                            use tsclientlib::events::{Event, PropertyId};
                            if let Event::PropertyRemoved { id: PropertyId::Client(id), .. } = &event {
                                let _ = audio.handle.control(Control::RemovePeer(id.0));
                            }
                            if state.status == ConnectionStatus::Connected
                                && let Some(book) = connection.as_ref().and_then(|con| con.get_state().ok())
                                && let Some(update) = presence(book, &event, state.generation, &ignored_uids)
                            {
                                let _ = events.send(update);
                            }
                            if let tsclientlib::events::Event::Message { target, invoker, message } = event {
                                let sender_uid = connection.as_ref().and_then(|con| con.get_state().ok())
                                    .and_then(|book|book.clients.get(&invoker.id))
                                    .and_then(|client|client.uid.as_ref())
                                    .map(|uid| uid.to_string());
                                if sender_uid.as_ref().is_some_and(|uid| ignored_uids.contains(uid)) { continue; }
                                let target=match target{MessageTarget::Channel=>"channel".into(),MessageTarget::Server=>"server".into(),MessageTarget::Client(id)=>format!("client:{}",if Some(id.0)==state.own_client_id{invoker.id.0}else{id.0}),MessageTarget::Poke(_)=>"poke".into()};
                                let _ = events.send(SessionEvent::Message { generation: state.generation, sender: invoker.name,sender_id:invoker.id.0, sender_uid, target, text: message });
                            }
                        }
                        if let Some(con) = &connection { synchronize(con, &mut state, channel_list_complete); }
                        sync_ignored(&state, &ignored_uids, &mut ignored_ids, &audio.handle);
                        if state.status == ConnectionStatus::Connected { attempt_started = None; queue_recovery = None; state.notice = None; }
                        audio.permissions(&connection);
                    }
                    Some(Ok(StreamItem::MessageEvent(tsclientlib::InMessage::ChannelListFinished(_)))) => {
                        channel_list_complete = true;
                        if let Some(con) = &connection { synchronize(con, &mut state, channel_list_complete); }
                        sync_ignored(&state, &ignored_uids, &mut ignored_ids, &audio.handle);
                        if state.status == ConnectionStatus::Connected { attempt_started = None; queue_recovery = None; state.notice = None; }
                        audio.permissions(&connection);
                    }
                    Some(Ok(StreamItem::IdentityLevelIncreasing(level))) => {
                        state.notice = Some(format!("正在提升身份安全等级至 {level}")); state.sequence += 1;
                    }
                    Some(Ok(StreamItem::IdentityLevelIncreased)) => {
                        if let Some(id) = connection.as_ref().and_then(|con| con.get_options().get_identity()).cloned()
                            && !matches!(active_identity_mode, IdentityMode::Ephemeral) {
                                let mode = active_identity_mode;
                                saving_identity.spawn_blocking(move || identities::save(mode, &id));
                        }
                    }
                    Some(Ok(event @ (StreamItem::FileDownload(..) | StreamItem::FileUpload(..) | StreamItem::FiletransferFailed(..)
                        | StreamItem::MessageEvent(tsclientlib::InMessage::FileList(_))
                        | StreamItem::MessageEvent(tsclientlib::InMessage::FileListFinished(_))))) => {
                        files.event(event);
                        continue;
                    }
                    Some(Ok(StreamItem::MessageResult(handle, result))) => {
                        files.acknowledge(handle, result.as_ref().err());
                        bans.acknowledge(handle, result.as_ref().err());
                        if let Some((confirmation, _)) = pending.remove(&handle.0) {
                            confirmation.finish(&events, result.map_err(|error| error.to_string()));
                        }
                    }
                    Some(Ok(_)) => continue,
                    terminal => {
                        // Drop before any more polling. The upstream completed future is not fused.
                        keyboard_ptt = false; button_ptt = false; hotkey.borrow_and_update();
                        audio.stop();
                        connection = None;
                        queue_recovery = None;
                        ignored_ids.clear();
                        attempt_started = None;
                        files.disconnected();
                        bans.disconnected();
                        fail_pending(&mut pending, &events);
                        state.clear_server();
                        state.notice = None;
                        state.last_error = Some(match terminal { Some(Err(error)) => error.to_string(), _ => "服务器已断开连接".into() });
                        state.transition(ConnectionStatus::Failed);
                    }
                }
                publish(&mut state, &tx, &events);
            }
            _ = tick.tick() => {
                files.expire();
                bans.expire();
                if preparing.as_ref().is_some_and(|p|p.started.elapsed()>Duration::from_secs(10)) {
                    let preparation=preparing.take().unwrap();
                    let error="读取系统身份超时（10 秒），已停止本次连接；请检查系统凭据授权".to_string();
                    let _=preparation.reply.send(Err(error.clone()));state.notice=None;state.last_error=Some(error);state.transition(ConnectionStatus::Failed);
                    publish(&mut state,&tx,&events);
                }
                if audio.enabled {
                    state.network = connection.as_ref().and_then(|c|c.get_network_stats().ok()).map(|stats| {
                        use tsclientlib::PacketStat::{InSpeech, OutControl};
                        congspeak_core::NetworkStats {
                            round_trip_ms: stats.rtt.as_secs_f64() * 1000.,
                            variation_ms: stats.rtt_dev.as_secs_f64() * 1000.,
                            inbound_voice_loss: stats.get_packetloss_s2c_speech(),
                            inbound_loss: stats.get_packetloss_s2c_total(),
                            inbound_bytes: stats.total_bytes[..=InSpeech as usize].iter().copied().sum(),
                            outbound_bytes: stats.total_bytes[OutControl as usize..].iter().copied().sum(),
                        }
                    });
                    state.audio=audio.handle.status();
                    audio.sync_flags(&mut connection);
                    audio.permissions(&connection);
                    state.audio.sent_packets=audio.sent_packets;
                    state.audio.transmitting &= audio.last_sent.is_some_and(|time| time.elapsed()<Duration::from_millis(250));
                    state.audio.send_block_reason=audio.send_error.clone().or_else(|| audio.send_block_reason(&connection));
                    if let Some(con)=connection.as_ref()
                        && let Ok(book)=con.get_state()
                            && let Some(own)=book.clients.get(&book.own_client)
                            && let Some(channel)=book.channels.get(&own.channel)
                            && !format!("{:?}",channel.codec).starts_with("Opus") {
                            state.audio.processing_error=Some("当前频道编码不受支持，请使用 Opus 频道".into());
                    }
                    publish(&mut state,&tx,&events);
                }

                let expired: Vec<_> = pending.iter().filter(|(_, (_, time))| time.elapsed() > Duration::from_secs(10)).map(|(key, _)| *key).collect();
                for key in expired {
                    if let Some((confirmation, _)) = pending.remove(&key) {
                        confirmation.finish(&events, Err("等待服务器确认超时，发送结果未知".into()));
                    }
                }
                let limit = if queue_recovery.is_some() { QUEUE_RECOVERY_TIMEOUT } else { Duration::from_secs(30) };
                if attempt_started.is_some_and(|time| time.elapsed() > limit) {
                    keyboard_ptt = false; button_ptt = false; hotkey.borrow_and_update();
                    audio.stop();
                    close(&mut connection).await;
                    files.disconnected();
                    bans.disconnected();
                    fail_pending(&mut pending, &events);
                    ignored_ids.clear();
                    attempt_started = None;
                    let was_recovering = queue_recovery.take().is_some();
                    state.clear_server();
                    state.notice = None;
                    state.last_error = Some(if was_recovering {
                        "接收队列溢出后重连超时，请重新连接服务器".into()
                    } else {
                        "连接超时（30 秒），已停止尝试".into()
                    });
                    state.transition(ConnectionStatus::Failed); publish(&mut state, &tx, &events);
                } else if let Some(recovery) = &mut queue_recovery
                    && recovery.retry_at.is_some_and(|time| time <= tokio::time::Instant::now())
                {
                    recovery.retry_at = None;
                    match recovery.options.clone().connect() {
                        Ok(fresh) => connection = Some(fresh),
                        Err(error) => {
                            queue_recovery = None;
                            attempt_started = None;
                            audio.stop();
                            state.notice = None;
                            state.last_error = Some(error.to_string());
                            state.transition(ConnectionStatus::Failed);
                            publish(&mut state, &tx, &events);
                        }
                    }
                }
            }
        }
    }
}

fn presence(
    book: &tsclientlib::data::Connection,
    event: &tsclientlib::events::Event,
    generation: u32,
    ignored: &HashSet<String>,
) -> Option<SessionEvent> {
    use tsclientlib::{
        Reason,
        events::{Event, PropertyId, PropertyValue},
    };
    let (client, joined) = match event {
        Event::PropertyAdded {
            id: PropertyId::Client(id),
            extra,
            ..
        } if extra.reason == Some(Reason::None) => (book.clients.get(id)?, true),
        Event::PropertyRemoved {
            old: PropertyValue::Client(client),
            extra,
            ..
        } if matches!(
            extra.reason,
            Some(
                Reason::Clientdisconnect
                    | Reason::LostConnection
                    | Reason::KickServer
                    | Reason::KickServerBan
            )
        ) =>
        {
            (client, false)
        }
        _ => return None,
    };
    if client.id == book.own_client
        || client
            .uid
            .as_ref()
            .is_some_and(|uid| ignored.contains(&uid.to_string()))
    {
        return None;
    }
    Some(SessionEvent::Presence {
        generation,
        joined,
        name: client.name.clone(),
        channel: book
            .channels
            .get(&client.channel)
            .map_or_else(|| format!("#{}", client.channel.0), |ch| ch.name.clone()),
    })
}

fn sync_ignored(
    state: &Snapshot,
    uids: &HashSet<String>,
    ids: &mut HashSet<u16>,
    audio: &AudioHandle,
) {
    let current: HashSet<_> = state
        .clients
        .iter()
        .filter(|client| {
            Some(client.id) != state.own_client_id
                && client.uid.as_ref().is_some_and(|uid| uids.contains(uid))
        })
        .map(|client| client.id)
        .collect();
    if current != *ids {
        *ids = current.clone();
        let _ = audio.control(Control::IgnoreClients(current));
    }
}

struct PendingIdentity {
    task: tokio::task::JoinHandle<Result<Identity, String>>,
    request: ConnectRequest,
    reply: Reply,
    mode: IdentityMode,
    started: tokio::time::Instant,
}
async fn next_identity(pending: &mut Option<PendingIdentity>) -> Result<Identity, String> {
    match pending {
        Some(p) => (&mut p.task)
            .await
            .map_err(|_| "身份读取任务异常".to_string())?,
        None => std::future::pending().await,
    }
}

struct AudioSession {
    handle: AudioHandle,
    outgoing: mpsc::Receiver<EncodedAudio>,
    settings: AudioSettings,
    stored: Arc<Mutex<AudioSettings>>,
    enabled: bool,
    epoch: u64,
    ptt: bool,
    mic_test: bool,
    permissions: (bool, bool),
    last_flags: Option<(bool, bool, bool, bool)>,
    channel: Option<String>,
    sent_packets: u64,
    last_sent: Option<std::time::Instant>,
    last_permissions: Option<(bool, bool, Option<bool>, u8, u64)>,
    send_error: Option<String>,
}
impl AudioSession {
    fn set_ptt(&mut self, connection: &mut Option<Connection>, held: bool) {
        if self.settings.talk_mode != TalkMode::PushToTalk || self.ptt == held {
            return;
        }
        self.epoch += 1;
        self.ptt = held;
        let _ = self.handle.control(Control::Ptt(held, self.epoch));
        if !held {
            self.end(connection);
        }
    }

    fn suspend_transport(&mut self) {
        self.ptt = false;
        self.permissions = (false, false);
        self.last_flags = None;
        self.epoch += 1;
        self.last_sent = None;
        self.send_error = None;
        let _ = self.handle.control(Control::Ptt(false, self.epoch));
        self.permissions(&None);
        let _ = self.handle.control(Control::ClearPeers);
    }

    fn stop(&mut self) {
        self.epoch += 1;
        self.ptt = false;
        self.mic_test = false;
        self.permissions = (false, false);
        self.last_flags = None;
        self.last_permissions = None;
        self.send_error = None;
        self.channel = None;
        self.sent_packets = 0;
        self.last_sent = None;
        let _ = self.handle.control(Control::Stop);
    }
    fn end(&self, con: &mut Option<Connection>) {
        if let Some(con) = con.as_mut().filter(|con| con.can_send_audio()) {
            let _ = con.send_audio(OutAudio::new(&AudioData::C2S {
                id: 0,
                codec: if self
                    .last_permissions
                    .is_some_and(|(_, _, music, _, _)| music == Some(true))
                {
                    CodecType::OpusMusic
                } else {
                    CodecType::OpusVoice
                },
                data: &[],
            }));
        }
    }
    fn send_block_reason(&self, connection: &Option<Connection>) -> Option<String> {
        let status = self.handle.status();
        let reason = if connection.is_none() {
            "尚未连接服务器"
        } else if self.mic_test {
            "正在本机试听，已暂停网络发言"
        } else if self.settings.input_muted {
            "麦克风已静音"
        } else if self.settings.output_muted {
            "耳机已静音，同时停止发送"
        } else if !status.input_ready {
            "麦克风尚未就绪，请查看设备错误"
        } else if !status.output_ready {
            "播放设备尚未就绪，无法启用语音发送"
        } else if let Some(con) = connection
            && let Ok(book) = con.get_state()
            && let Some(own) = book.clients.get(&book.own_client)
            && own.away_message.is_some()
        {
            "离开状态中，返回后才能发言"
        } else if !self.permissions.0 {
            "服务器尚未允许发言，请检查频道发言权和本人的静音状态"
        } else if status.processing_error.is_some() {
            "声音处理出现异常，正在自动恢复"
        } else if self.settings.talk_mode == TalkMode::PushToTalk && !self.ptt {
            return Some(format!(
                "麦克风已开启，按住 {} 或发言按钮才会发送",
                self.settings.ptt_key
            ));
        } else if self.settings.talk_mode == TalkMode::VoiceActivation && !status.transmitting {
            "等待检测到人声"
        } else {
            return None;
        };
        Some(reason.into())
    }
    fn permissions(&mut self, connection: &Option<Connection>) {
        if !self.enabled {
            return;
        }
        if let Some(con) = connection {
            self.permissions = (con.can_send_audio(), con.can_receive_audio());
        }
        let (mut music, mut quality) = (None, 7);
        let mut current_channel = None;
        if let Some(con) = connection
            && let Ok(book) = con.get_state()
            && let Some(client) = book.clients.get(&book.own_client)
            && let Some(channel) = book.channels.get(&client.channel)
        {
            current_channel = Some(channel.id.0.to_string());
            let codec = format!("{:?}", channel.codec);
            music = match codec.as_str() {
                "OpusVoice" => Some(false),
                "OpusMusic" => Some(true),
                _ => None,
            };
            quality = channel.codec_quality.unwrap_or(7);
        }
        if current_channel != self.channel {
            self.epoch += 1;
            self.channel = current_channel;
        }
        let desired = (
            self.permissions.0,
            self.permissions.1,
            music,
            quality,
            self.epoch,
        );
        if self.last_permissions == Some(desired) {
            return;
        }
        if self
            .handle
            .control(Control::Permissions {
                send: self.permissions.0,
                receive: self.permissions.1,
                music,
                quality,
                epoch: self.epoch,
            })
            .is_ok()
        {
            self.last_permissions = Some(desired);
        }
    }
    fn sync_flags(&mut self, connection: &mut Option<Connection>) {
        if !self.enabled {
            return;
        }
        let status = self.handle.status();
        let flags = (
            self.settings.input_muted || self.mic_test,
            self.settings.output_muted,
            status.input_ready,
            status.output_ready,
        );
        if let Some(con) = connection
            && let Ok(book) = con.get_state()
            && let Some(own) = book.clients.get(&book.own_client)
        {
            // Do not publish device flags before the own-client entry exists: an initial
            // server snapshot can otherwise overwrite the local update without a change event.
            let actual = (
                own.input_muted,
                own.output_muted,
                own.input_hardware_enabled,
                own.output_hardware_enabled,
            );
            if self.last_flags == Some(flags) && actual == flags {
                return;
            }
            let part = book
                .client_update()
                .set_input_muted(flags.0)
                .set_output_muted(flags.1)
                .set_input_hardware_enabled(flags.2)
                .set_output_hardware_enabled(flags.3);
            if part.to_packet().send(con).is_ok() {
                self.last_flags = Some(flags);
            }
        }
    }
}

async fn next_encoded(receiver: &mut mpsc::Receiver<EncodedAudio>) -> EncodedAudio {
    match receiver.recv().await {
        Some(frame) => frame,
        None => std::future::pending().await,
    }
}

fn submit(
    con: &mut Connection,
    state: &Snapshot,
    target: &RequestTarget,
    op: &Operation,
) -> Result<tsclientlib::MessageHandle, String> {
    if state.status != ConnectionStatus::Connected {
        return Err("服务器正在同步或重连，请稍后重试".into());
    }
    let book = con.get_state().map_err(|e| e.to_string())?;
    if let RequestTarget::Confirm { generation, peer } = target {
        if *generation != state.generation {
            return Err("连接已变化，请重新选择操作对象".into());
        }
        if let Some((id, uid)) = peer
            && !book
                .clients
                .get(&tsclientlib::ClientId(*id))
                .and_then(|c| c.uid.as_ref())
                .is_some_and(|current| current.to_string() == *uid)
        {
            return Err("用户已离开或身份已变化，请重新选择用户".into());
        }
    }
    let packet = match op {
        Operation::Action(action) => {
            if let ClientAction::RemoveBan { generation, .. } = action
                && *generation != state.generation
            {
                return Err("服务器连接已变化，请重新打开封禁列表".into());
            }
            build_action(con, action)?
        }
        Operation::Message(target, text) => book.send_message(
            match target {
                ChatTarget::Channel => MessageTarget::Channel,
                ChatTarget::Server => MessageTarget::Server,
                ChatTarget::Client { client_id } => {
                    let id = tsclientlib::ClientId(*client_id);
                    if !book.clients.contains_key(&id) {
                        return Err("用户已离线或不可见".into());
                    }
                    MessageTarget::Client(id)
                }
            },
            text,
        ),
    };
    packet.send_with_result(con).map_err(|e| e.to_string())
}

fn build_action(
    con: &Connection,
    action: &ClientAction,
) -> Result<tsproto_packets::packets::OutCommand, String> {
    use tsclientlib::{
        ChannelId, ClientId, Reason,
        data::exts::*,
        messages::{OutMessageTrait, c2s},
    };
    let book = con.get_state().map_err(|e| e.to_string())?;
    let channel_id = |id: &String| {
        id.parse::<u64>()
            .map(ChannelId)
            .map_err(|_| "频道标识无效".to_string())
    };
    let known_channel = |id: &String| {
        book.channels
            .get(&channel_id(id)?)
            .ok_or_else(|| "频道不存在或不可见".to_string())
    };
    let parent_channel = |id: &String| {
        let id = channel_id(id)?;
        if id.0 == 0 || book.channels.contains_key(&id) {
            Ok(id)
        } else {
            Err("上级频道不存在或不可见".to_string())
        }
    };
    let client = |id: u16| {
        book.clients
            .get(&ClientId(id))
            .ok_or_else(|| "用户已离线或不可见".to_string())
    };
    let text = |s: &str, max: usize| {
        if s.len() > max || s.chars().any(|c| c == '\0') {
            Err("文本过长或包含无效字符".to_string())
        } else {
            Ok(())
        }
    };
    Ok(match action {
        ClientAction::Join {
            channel_id: id,
            password,
        } => {
            let mut part = client(book.own_client.0)?.client_move(channel_id(id)?);
            if let Some(p) = password {
                part = part.set_password(p);
            }
            part.to_packet()
        }
        ClientAction::Move {
            client_id,
            channel_id: id,
            password,
        } => {
            let mut part = client(*client_id)?.client_move(channel_id(id)?);
            if let Some(p) = password {
                part = part.set_password(p);
            }
            part.to_packet()
        }
        ClientAction::Rename { name } => {
            text(name, 64)?;
            if name.trim().is_empty() {
                return Err("昵称不能为空".into());
            }
            book.client_update().set_name(name).to_packet()
        }
        ClientAction::Away { message } => {
            if let Some(m) = message {
                text(m, 512)?;
            }
            book.client_update()
                .set_away(message.as_deref())
                .to_packet()
        }
        ClientAction::Subscribe { enabled } => book.server.set_subscribed(*enabled),
        ClientAction::Poke { client_id, message } => {
            text(message, 512)?;
            client(*client_id)?.poke(message)
        }
        ClientAction::Kick {
            client_id,
            from_server,
            reason,
        } => {
            text(reason, 512)?;
            client(*client_id)?
                .kick(if *from_server {
                    Reason::KickServer
                } else {
                    Reason::KickChannel
                })
                .set_reason_message(reason)
                .to_packet()
        }
        ClientAction::EditChannel {
            channel_id: id,
            name,
            topic,
            description,
        } => {
            text(name, 128)?;
            text(topic, 1024)?;
            if let Some(description) = description {
                text(description, 8192)?;
            }
            if name.trim().is_empty() {
                return Err("频道名称不能为空".into());
            }
            let mut edit = known_channel(id)?.edit().set_name(name).set_topic(topic);
            if let Some(description) = description {
                edit = edit.set_description(description);
            }
            edit.to_packet()
        }
        ClientAction::CreateChannel {
            parent_id,
            name,
            topic,
            description,
            password,
            lifetime,
        } => {
            text(name, 128)?;
            text(topic, 1024)?;
            text(description, 8192)?;
            if name.trim().is_empty() {
                return Err("频道名称不能为空".into());
            }
            let kind = match lifetime {
                congspeak_core::ChannelLifetime::Temporary => tsclientlib::ChannelType::Temporary,
                congspeak_core::ChannelLifetime::SemiPermanent => {
                    tsclientlib::ChannelType::SemiPermanent
                }
                congspeak_core::ChannelLifetime::Permanent => tsclientlib::ChannelType::Permanent,
            };
            let parent = parent_channel(parent_id)?;
            let mut options = tsclientlib::data::ChannelOptions::new(name)
                .parent_id(parent)
                .channel_type(kind);
            if !topic.is_empty() {
                options = options.topic(topic);
            }
            if !description.is_empty() {
                options = options.description(description);
            }
            if let Some(password) = password {
                text(password, 512)?;
                options = options.password(password);
            }
            book.server.add_channel(options)
        }
        ClientAction::MoveChannel {
            channel_id: id,
            parent_id,
            previous_id,
        } => {
            let channel = known_channel(id)?;
            let parent = parent_channel(parent_id)?;
            let mut ancestor = parent;
            for _ in 0..=book.channels.len() {
                if ancestor == channel.id {
                    return Err("不能把频道移入自身或其子频道".into());
                }
                let Some(current) = book.channels.get(&ancestor) else {
                    break;
                };
                ancestor = current.parent;
            }
            let order = previous_id.as_ref().map(channel_id).transpose()?;
            if let Some(previous) = order
                && previous.0 != 0
                && (previous == channel.id
                    || !book
                        .channels
                        .get(&previous)
                        .is_some_and(|c| c.parent == parent))
            {
                return Err("排序参照必须是同级的另一个频道".into());
            }
            c2s::OutChannelMovePart {
                channel_id: channel.id,
                parent_id: parent,
                order,
            }
            .to_packet()
        }
        ClientAction::DeleteChannel { channel_id } => c2s::OutChannelDeletePart {
            channel_id: known_channel(channel_id)?.id,
            force: false,
        }
        .to_packet(),
        ClientAction::SubscribeChannel {
            channel_id,
            enabled,
        } => known_channel(channel_id)?.set_subscribed(*enabled),
        ClientAction::ChannelPassword {
            channel_id,
            password,
        } => {
            if let Some(password) = password {
                text(password, 512)?;
            }
            known_channel(channel_id)?
                .edit()
                .set_password(password.as_deref())
                .to_packet()
        }
        ClientAction::ClientDescription { description } => {
            text(description, 8192)?;
            client(book.own_client.0)?
                .edit()
                .set_description(description)
                .to_packet()
        }
        ClientAction::RequestTalk { message } => {
            if let Some(message) = message {
                text(message, 512)?;
            }
            book.client_update()
                .set_talk_power_request(message.as_deref())
                .to_packet()
        }
        ClientAction::GrantTalk { client_id, granted } => client(*client_id)?
            .edit()
            .set_talk_power_granted(*granted)
            .to_packet(),
        ClientAction::BanClient {
            client_id,
            minutes,
            reason,
        } => {
            text(reason, 512)?;
            if *minutes == 0 {
                return Err("封禁时长必须大于零".into());
            }
            if *client_id == book.own_client.0 {
                return Err("不能封禁自己".into());
            }
            c2s::OutBanClientPart {
                client_id: client(*client_id)?.id,
                time: Some(
                    Duration::from_secs(u64::from(*minutes) * 60)
                        .try_into()
                        .map_err(|_| "封禁时长无效")?,
                ),
                ban_reason: Some(reason.as_str().into()),
            }
            .to_packet()
        }
        ClientAction::RemoveBan { ban_id, .. } => {
            c2s::OutBanDelPart { ban_id: *ban_id }.to_packet()
        }
        ClientAction::Complaint {
            client_id,
            uid,
            message,
        } => {
            let target = client(*client_id)?;
            if target.id == book.own_client
                || target.uid.as_ref().is_none_or(|id| id.to_string() != *uid)
            {
                return Err("投诉对象已变化，请重新选择用户".into());
            }
            if message.trim().is_empty() {
                return Err("投诉内容不能为空".into());
            }
            text(message, 512)?;
            c2s::OutComplainAddPart {
                target_client_db_id: target.database_id,
                message: message.as_str().into(),
            }
            .to_packet()
        }
        ClientAction::RedeemKey { key } => {
            text(key, 1024)?;
            if key.is_empty() {
                return Err("密钥不能为空".into());
            }
            c2s::OutTokenUsePart {
                token: key.as_str().into(),
            }
            .to_packet()
        }
    })
}

#[cfg(test)]
mod preparation_tests {
    use super::*;
    fn queue_error(queue: &'static str) -> tsclientlib::Error {
        tsclientlib::Error::ConnectionFailed(tsproto::client::Error::TsProto(
            tsproto::Error::MaxLengthExceeded(queue),
        ))
    }

    #[test]
    fn only_receive_queue_overflows_reopen_the_transport() {
        assert!(receive_queue_overflow(&queue_error("command queue")));
        assert!(receive_queue_overflow(&queue_error("fragment queue")));
        assert!(!receive_queue_overflow(&queue_error("encryption data")));
        assert!(!receive_queue_overflow(&tsclientlib::Error::ConnectionGone));
    }

    #[tokio::test]
    #[ignore = "Requires an authorized and otherwise empty TS3 test channel and local audio devices"]
    async fn receive_queue_overflow_reconnects_and_resumes_voice()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::io::Read;

        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        let request: ConnectRequest = serde_json::from_str(&input)?;
        let channel = request
            .channel
            .as_deref()
            .and_then(|value| value.strip_prefix('/'))
            .ok_or("请用 /频道ID 明确指定获授权的测试频道")?;
        let settings = AudioSettings {
            input_gain: 0.,
            echo_cancellation: false,
            auto_gain: false,
            noise_mode: congspeak_core::NoiseMode::Off,
            input_muted: true,
            talk_mode: TalkMode::Continuous,
            ..Default::default()
        };
        let session = Session::start_with_audio(IdentityMode::Ephemeral, settings.clone());
        let mut state = session.watch();
        let result = tokio::time::timeout(Duration::from_secs(55), async {
            session.connect(request.clone()).await?;
            let before = state
                .wait_for(|s| {
                    matches!(
                        s.status,
                        ConnectionStatus::Connected | ConnectionStatus::Failed
                    )
                })
                .await?
                .clone();
            if let Some(error) = &before.last_error {
                return Err(format!("初次连接：{error}").into());
            }
            eprintln!("initial connection synchronized");
            let before_own = before
                .clients
                .iter()
                .find(|c| Some(c.id) == before.own_client_id)
                .ok_or("初次连接缺少本机用户")?;
            assert_eq!(before_own.channel_id, channel);
            assert!(before_own.uid.is_some());

            if before
                .clients
                .iter()
                .any(|c| c.channel_id == channel && c.uid != before_own.uid)
            {
                return Err("测试频道有其他人，已跳过发包".into());
            }
            session
                .set_audio_settings(AudioSettings {
                    input_muted: false,
                    ..settings.clone()
                })
                .await?;
            tokio::time::timeout(
                Duration::from_secs(8),
                state.wait_for(|s| s.audio.can_receive && s.audio.sent_packets >= 8),
            )
            .await
            .map_err(|_| format!("故障注入前无语音包提交：{:?}", session.snapshot().audio))??;
            let sent_before = session.snapshot().audio.sent_packets;
            eprintln!("initial voice packets submitted");

            // Inject the exact typed error at the boundary that receives transport results.
            session
                .transport_fault
                .send(queue_error("command queue"))
                .await?;
            let after = state
                .wait_for(|s| {
                    s.generation > before.generation
                        && matches!(
                            s.status,
                            ConnectionStatus::Connected | ConnectionStatus::Failed
                        )
                })
                .await?
                .clone();
            if let Some(error) = &after.last_error {
                return Err(format!("恢复连接：{error}").into());
            }
            eprintln!("recovered connection synchronized");
            let after_own = after
                .clients
                .iter()
                .find(|c| Some(c.id) == after.own_client_id)
                .ok_or("重连后缺少本机用户")?;
            assert_eq!(before_own.uid, after_own.uid);
            assert_eq!(after_own.channel_id, channel);
            assert!(after.last_error.is_none());
            assert!(
                session
                    .action_confirmed(
                        before.generation,
                        None,
                        ClientAction::Subscribe { enabled: true }
                    )
                    .await
                    .is_err()
            );
            session
                .action_confirmed(
                    after.generation,
                    None,
                    ClientAction::Subscribe { enabled: true },
                )
                .await?;

            assert!(!session.audio_settings().input_muted);
            tokio::time::timeout(
                Duration::from_secs(8),
                state.wait_for(|s| s.audio.can_receive && s.audio.sent_packets >= sent_before + 8),
            )
            .await
            .map_err(|_| format!("重连后语音未恢复：{:?}", session.snapshot().audio))??;
            eprintln!("voice packets resumed without changing audio settings");
            session.set_audio_settings(settings).await?;
            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await;
        let _ = session.disconnect().await;
        result?
    }

    #[tokio::test]
    async fn cancelling_pending_identity_does_not_wait_for_the_credential_provider() {
        let session = Session::start(IdentityMode::DelayedTest);
        let connecting = session.clone();
        let task = tokio::spawn(async move {
            connecting
                .connect(ConnectRequest {
                    address: "127.0.0.1".into(),
                    nickname: "Cancel-Test".into(),
                    password: None,
                    channel: None,
                    channel_password: None,
                })
                .await
        });
        tokio::time::timeout(Duration::from_millis(250), async {
            while session.snapshot().status != ConnectionStatus::Connecting {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_millis(150), session.disconnect())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.snapshot().status, ConnectionStatus::Disconnected);
        assert!(task.await.unwrap().is_err());
        tokio::time::sleep(Duration::from_millis(550)).await;
        assert_eq!(
            session.snapshot().status,
            ConnectionStatus::Disconnected,
            "a cancelled identity read must not initiate a later connection"
        );
    }
}
