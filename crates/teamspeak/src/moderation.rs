use crate::{Command, Session};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::oneshot, time::Instant};
use tsclientlib::{
    CommandError, Connection, MessageHandle, OutCommandExt, TsError,
    messages::c2s::OutBanListRequestMessage,
};
use tsproto::connection::Event;
use tsproto_packets::{
    commands::{CommandItem, CommandParser},
    packets::PacketType,
};

const MAX_BANS: usize = 10_000;

#[derive(Default)]
pub struct Ban {
    pub id: u32,
    pub name: String,
    pub ip: String,
    pub uid: String,
    pub seconds: u64,
    pub reason: String,
    pub author: String,
}

pub(crate) type BanReply = oneshot::Sender<Result<Vec<Ban>, String>>;
type Capture = Arc<Mutex<Option<Result<Vec<Ban>, String>>>>;

#[derive(Default)]
pub(crate) struct BanRequests {
    capture: Option<Capture>,
    pending: Option<(MessageHandle, Instant, BanReply)>,
    expired: bool,
}

impl Session {
    pub async fn bans(&self, generation: u32) -> Result<Vec<Ban>, String> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(Command::Bans(generation, reply))
            .await
            .map_err(|_| "会话任务已退出")?;
        result.await.map_err(|_| "会话任务已退出")?
    }
}

impl BanRequests {
    pub fn request(&mut self, connection: Option<&mut Connection>, reply: BanReply) {
        let result = self.start(connection);
        match result {
            Ok(handle) => self.pending = Some((handle, Instant::now(), reply)),
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    fn start(&mut self, connection: Option<&mut Connection>) -> Result<MessageHandle, String> {
        let con = connection.ok_or("服务器连接已变化，请重新打开封禁列表")?;
        if self.expired {
            return Err("上一次封禁查询已超时，请重新连接服务器后再试".into());
        }
        if self.pending.is_some() {
            return Err("已有封禁查询正在处理".into());
        }
        if self.capture.is_none() {
            let capture = Capture::default();
            let listener = capture.clone();
            // TS3 sends IP and name patterns in notifybanlist. The generated message decoder
            // expects a literal IP address, so read this response from the ordered command stream.
            con.get_tsproto_client_mut()
                .map_err(|error| error.to_string())?
                .event_listeners
                .push(Box::new(move |event| {
                    let Event::ReceivePacket(packet) = event else {
                        return;
                    };
                    if packet.header().packet_type() != PacketType::Command
                        || !packet.content().starts_with(b"notifybanlist ")
                    {
                        return;
                    }
                    let (name, args) = CommandParser::new(packet.content());
                    if name == b"notifybanlist"
                        && let Ok(mut capture) = listener.lock()
                        && let Some(Ok(entries)) = capture.as_mut()
                        && let Err(error) = append_bans(entries, args)
                    {
                        *capture = Some(Err(error));
                    }
                }));
            self.capture = Some(capture);
        }
        let capture = self
            .capture
            .as_ref()
            .expect("installed ban response listener");
        let handle = OutBanListRequestMessage::new()
            .send_with_result(con)
            .map_err(|error| error.to_string())?;
        *capture.lock().map_err(|_| "封禁列表状态异常")? = Some(Ok(Vec::new()));
        Ok(handle)
    }

    pub fn acknowledge(&mut self, handle: MessageHandle, error: Option<&CommandError>) {
        if !self
            .pending
            .as_ref()
            .is_some_and(|(id, _, _)| *id == handle)
        {
            return;
        }
        let (_, _, reply) = self.pending.take().expect("matched ban request");
        let capture = self
            .capture
            .as_ref()
            .and_then(|capture| capture.lock().ok().and_then(|mut content| content.take()));
        let result = match error {
            None => capture.unwrap_or_else(|| Err("封禁列表未返回".into())),
            Some(err) if err.error == TsError::DatabaseEmptyResult => Ok(Vec::new()),
            Some(err) => Err(err.to_string()),
        };
        let _ = reply.send(result);
    }

    pub fn expire(&mut self) {
        if self
            .pending
            .as_ref()
            .is_some_and(|(_, started, _)| started.elapsed() >= Duration::from_secs(10))
        {
            let (_, _, reply) = self.pending.take().expect("expired ban request");
            let _ = reply.send(Err("等待服务器返回封禁列表超时".into()));
            // An untagged late list must not be consumed by a later request.
            self.expired = true;
            if let Some(capture) = &self.capture
                && let Ok(mut content) = capture.lock()
            {
                *content = None;
            }
        }
    }

    pub fn disconnected(&mut self) {
        if let Some((_, _, reply)) = self.pending.take() {
            let _ = reply.send(Err("连接已断开".into()));
        }
        *self = Self::default();
    }
}

fn append_bans(entries: &mut Vec<Ban>, args: CommandParser<'_>) -> Result<(), String> {
    let mut row = Ban::default();
    let mut has_name = false;
    for item in args.chain(std::iter::once(CommandItem::NextCommand)) {
        match item {
            CommandItem::Argument(arg) => {
                let value = arg.value();
                let decode = || {
                    value
                        .get_str()
                        .map(|s| s.into_owned())
                        .map_err(|_| "封禁列表含有无法读取的文本".to_owned())
                };
                match arg.name() {
                    b"banid" => row.id = decode()?.parse().map_err(|_| "封禁编号无效")?,
                    b"name" if !has_name && row.name.is_empty() => row.name = decode()?,
                    b"lastnickname" => {
                        let nickname = decode()?;
                        if !nickname.is_empty() {
                            has_name = true;
                            row.name = nickname;
                        }
                    }
                    b"ip" => row.ip = decode()?,
                    b"uid" => row.uid = decode()?,
                    b"duration" => row.seconds = decode()?.parse().map_err(|_| "封禁时长无效")?,
                    b"reason" => row.reason = decode()?,
                    b"invokername" => row.author = decode()?,
                    _ => {}
                }
            }
            CommandItem::NextCommand => {
                if row.id == 0 {
                    return Err("封禁列表缺少编号".into());
                }
                if entries.len() >= MAX_BANS {
                    return Err("封禁列表超出客户端显示上限".into());
                }
                entries.push(std::mem::take(&mut row));
                has_name = false;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ip_patterns_and_multiple_bans() {
        let (_, args) = CommandParser::new(br"notifybanlist banid=3 ip=42\\.42\\.42\\.42 name=test lastnickname=Example\sUser uid=abc duration=12 reason=Test\sreason|banid=4 ip= uid=def duration=0 reason=");
        let mut entries = Vec::new();
        append_bans(&mut entries, args).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].ip, r"42\.42\.42\.42");
        assert_eq!(entries[0].name, "Example User");
        assert_eq!(entries[0].reason, "Test reason");
        assert_eq!(entries[1].id, 4);
        assert_eq!(entries[1].uid, "def");
    }
}
