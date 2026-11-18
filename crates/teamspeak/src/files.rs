use crate::Session;
use congspeak_core::ConnectionStatus;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::{oneshot, watch},
    time::{Instant, timeout},
};
use tsclientlib::{
    ChannelId, CommandError, Connection, InMessage, MessageHandle, OutCommandExt, StreamItem,
    messages::{OutMessageTrait, c2s},
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const IO_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub is_file: bool,
    pub modified: String,
}

#[derive(Clone)]
pub struct Location {
    pub generation: u32,
    pub channel: String,
    pub path: String,
    pub password: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Progress {
    pub bytes: u64,
    pub total: u64,
}

pub enum Mutation {
    CreateDirectory,
    Delete,
    Rename { destination: String },
}

type Answer<T> = oneshot::Sender<Result<T, String>>;

pub(crate) enum Command {
    List(Location, Answer<Vec<Entry>>),
    Mutate(Location, Mutation, Answer<()>),
    Transfer(Location, Option<u64>, Answer<Socket>),
}

pub(crate) struct Socket {
    stream: TcpStream,
    size: u64,
}

struct Listing {
    handle: MessageHandle,
    channel: ChannelId,
    path: String,
    reply: Answer<Vec<Entry>>,
    entries: Vec<Entry>,
    started: Instant,
    complete: bool,
    acknowledged: bool,
}

#[derive(Default)]
pub(crate) struct Requests {
    listing: Option<Listing>,
    mutations: HashMap<u16, (Instant, Answer<()>)>,
    transfers: HashMap<u16, (Instant, Answer<Socket>)>,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !matches!(name, "." | "..")
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control)
}

pub fn child_path(parent: &str, name: &str) -> Result<String, String> {
    if !valid_name(name) {
        return Err("文件名称无效".into());
    }
    let full = format!("{}/{name}", parent.trim_end_matches('/'));
    validate_path(&full, false)?;
    Ok(full)
}

fn validate_path(path: &str, allow_root: bool) -> Result<(), String> {
    if path == "/" && allow_root {
        return Ok(());
    }
    if path.len() <= 4096
        && path
            .strip_prefix('/')
            .is_some_and(|part| part.split('/').all(valid_name))
    {
        Ok(())
    } else {
        Err("请输入以 / 开头的频道文件路径，路径不能含有空段或 .、..".into())
    }
}

fn channel(con: &Connection, location: &Location, allow_root: bool) -> Result<ChannelId, String> {
    validate_path(&location.path, allow_root)?;
    let id = ChannelId(location.channel.parse().map_err(|_| "频道 ID 无效")?);
    if !con
        .get_state()
        .map_err(|e| e.to_string())?
        .channels
        .contains_key(&id)
    {
        return Err("频道不存在或不可见".into());
    }
    Ok(id)
}

fn password(location: &Location) -> String {
    location
        .password
        .as_ref()
        .filter(|p| !p.is_empty())
        .map(|p| tsproto_types::crypto::encode_password(p.as_bytes()))
        .unwrap_or_default()
}

impl Requests {
    pub fn request(&mut self, con: Option<&mut Connection>, generation: u32, request: Command) {
        let location = match &request {
            Command::List(location, _)
            | Command::Mutate(location, _, _)
            | Command::Transfer(location, _, _) => location,
        };
        let Some(con) = con.filter(|_| location.generation == generation) else {
            let error = "服务器连接已变化，请重新打开频道文件".to_string();
            match request {
                Command::List(_, reply) => {
                    let _ = reply.send(Err(error));
                }
                Command::Mutate(_, _, reply) => {
                    let _ = reply.send(Err(error));
                }
                Command::Transfer(_, _, reply) => {
                    let _ = reply.send(Err(error));
                }
            }
            return;
        };
        match request {
            Command::List(location, reply) => {
                let sent = (|| {
                    if self.listing.is_some() {
                        return Err("上一项文件列表请求仍在处理".into());
                    }
                    let channel = channel(con, &location, true)?;
                    let handle = c2s::OutFileListRequestPart {
                        channel_id: channel,
                        channel_password: password(&location).into(),
                        path: location.path.as_str().into(),
                    }
                    .to_packet()
                    .send_with_result(con)
                    .map_err(|e| e.to_string())?;
                    Ok((channel, handle))
                })();
                match sent {
                    Ok((channel, handle)) => {
                        self.listing = Some(Listing {
                            handle,
                            channel,
                            path: location.path,
                            reply,
                            entries: vec![],
                            started: Instant::now(),
                            complete: false,
                            acknowledged: false,
                        })
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
            Command::Mutate(location, action, reply) => {
                let sent = (|| {
                    let channel_id = channel(con, &location, false)?;
                    let channel_password = password(&location).into();
                    let path = location.path.as_str().into();
                    let packet = match action {
                        Mutation::CreateDirectory => c2s::OutCreateDirectoryPart {
                            channel_id,
                            channel_password,
                            directory_name: path,
                        }
                        .to_packet(),
                        Mutation::Delete => c2s::OutDeleteFilePart {
                            channel_id,
                            channel_password,
                            name: path,
                        }
                        .to_packet(),
                        Mutation::Rename { destination } => {
                            validate_path(&destination, false)?;
                            c2s::OutRenameFilePart {
                                channel_id,
                                channel_password,
                                target_channel_id: None,
                                target_channel_password: None,
                                old_name: path,
                                new_name: destination.into(),
                            }
                            .to_packet()
                        }
                    };
                    packet.send_with_result(con).map_err(|e| e.to_string())
                })();
                match sent {
                    Ok(handle) => {
                        self.mutations.insert(handle.0, (Instant::now(), reply));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
            Command::Transfer(location, size, reply) => {
                let sent = channel(con, &location, false).and_then(|id| {
                    let password = location.password.as_deref().filter(|p| !p.is_empty());
                    match size {
                        Some(size) => {
                            con.upload_file(id, &location.path, password, size, false, false)
                        }
                        None => con.download_file(id, &location.path, password, None),
                    }
                    .map_err(|e| e.to_string())
                });
                match sent {
                    Ok(handle) => {
                        self.transfers.insert(handle.0, (Instant::now(), reply));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
        }
    }

    pub fn acknowledge(&mut self, handle: MessageHandle, error: Option<&CommandError>) {
        if let Some((_, reply)) = self.mutations.remove(&handle.0) {
            let _ = reply.send(error.map_or(Ok(()), |e| Err(e.to_string())));
            return;
        }
        if self
            .listing
            .as_ref()
            .is_some_and(|job| job.handle.0 == handle.0)
        {
            if let Some(error) = error {
                let result = if error.error == tsclientlib::TsError::DatabaseEmptyResult {
                    Ok(Vec::new())
                } else {
                    Err(error.to_string())
                };
                let _ = self.listing.take().unwrap().reply.send(result);
            } else if let Some(listing) = &mut self.listing {
                listing.acknowledged = true;
                self.finish_listing();
            }
        }
    }

    pub fn event(&mut self, event: StreamItem) {
        match event {
            StreamItem::MessageEvent(InMessage::FileList(message)) => {
                if let Some(job) = &mut self.listing {
                    if message
                        .return_code
                        .as_ref()
                        .is_some_and(|code| code.parse() != Ok(job.handle.0))
                    {
                        return;
                    }
                    for entry in message
                        .iter()
                        .filter(|e| e.channel_id == job.channel && same_path(&e.path, &job.path))
                    {
                        if job.entries.len() >= 20_000 {
                            let _ = self
                                .listing
                                .take()
                                .unwrap()
                                .reply
                                .send(Err("目录超过 20,000 项，请使用较小的子目录".into()));
                            return;
                        }
                        if valid_name(&entry.name) {
                            job.entries.push(Entry {
                                name: entry.name.clone(),
                                size: entry.size,
                                is_file: entry.is_file,
                                modified: entry.date_time.to_string(),
                            });
                        }
                    }
                }
            }
            StreamItem::MessageEvent(InMessage::FileListFinished(message)) => {
                if let Some(job) = &mut self.listing
                    && message
                        .iter()
                        .any(|e| e.channel_id == job.channel && same_path(&e.path, &job.path))
                {
                    job.complete = true;
                    self.finish_listing();
                }
            }
            StreamItem::FileDownload(handle, result) => {
                self.complete_transfer(
                    handle.0,
                    Ok(Socket {
                        stream: result.stream,
                        size: result.size,
                    }),
                );
            }
            StreamItem::FileUpload(handle, result) => {
                let socket = if result.seek_position == 0 {
                    Ok(Socket {
                        stream: result.stream,
                        size: 0,
                    })
                } else {
                    Err("服务器为全新上传返回了非零起始位置".into())
                };
                self.complete_transfer(handle.0, socket);
            }
            StreamItem::FiletransferFailed(handle, error) => {
                self.complete_transfer(handle.0, Err(error.to_string()))
            }
            _ => {}
        }
    }

    fn finish_listing(&mut self) {
        if self
            .listing
            .as_ref()
            .is_some_and(|job| job.complete && job.acknowledged)
        {
            let mut job = self.listing.take().unwrap();
            job.entries
                .sort_by(|a, b| a.is_file.cmp(&b.is_file).then(a.name.cmp(&b.name)));
            let _ = job.reply.send(Ok(job.entries));
        }
    }

    fn complete_transfer(&mut self, handle: u16, socket: Result<Socket, String>) {
        if let Some((_, reply)) = self.transfers.remove(&handle) {
            let _ = reply.send(socket);
        }
    }

    pub fn expire(&mut self) {
        if self
            .listing
            .as_ref()
            .is_some_and(|job| job.started.elapsed() >= REQUEST_TIMEOUT)
        {
            let _ = self
                .listing
                .take()
                .unwrap()
                .reply
                .send(Err("等待服务器目录列表超时".into()));
        }
        expire(&mut self.mutations);
        expire(&mut self.transfers);
    }

    pub fn disconnected(&mut self) {
        let error = "服务器连接中断";
        if let Some(job) = self.listing.take() {
            let _ = job.reply.send(Err(error.into()));
        }
        for (_, (_, reply)) in self.mutations.drain() {
            let _ = reply.send(Err(error.into()));
        }
        for (_, (_, reply)) in self.transfers.drain() {
            let _ = reply.send(Err(error.into()));
        }
    }
}

fn same_path(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

fn expire<T>(jobs: &mut HashMap<u16, (Instant, Answer<T>)>) {
    for (_, (_, reply)) in jobs
        .extract_if(|_, (started, reply)| reply.is_closed() || started.elapsed() >= REQUEST_TIMEOUT)
    {
        let _ = reply.send(Err("等待服务器文件操作超时".into()));
    }
}

impl Session {
    async fn file_request<T>(&self, make: impl FnOnce(Answer<T>) -> Command) -> Result<T, String> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(crate::Command::Files(make(reply)))
            .await
            .map_err(|_| "会话任务已退出")?;
        result.await.map_err(|_| "会话任务已退出")?
    }

    pub async fn file_list(&self, location: Location) -> Result<Vec<Entry>, String> {
        self.file_request(|reply| Command::List(location, reply))
            .await
    }

    pub async fn file_mutation(&self, location: Location, action: Mutation) -> Result<(), String> {
        self.file_request(|reply| Command::Mutate(location, action, reply))
            .await
    }

    pub async fn download(
        &self,
        location: Location,
        destination: PathBuf,
        progress: watch::Sender<Progress>,
    ) -> Result<(), String> {
        self.during_connection(async {
            let parent = destination
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            if tokio::fs::try_exists(&destination)
                .await
                .map_err(|e| e.to_string())?
            {
                return Err("目标文件已存在，请选择另一个路径".into());
            }
            let temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
            let (file, path) = temporary.into_parts();
            let mut file = tokio::fs::File::from_std(file);
            let mut socket = self
                .file_request(|reply| Command::Transfer(location, None, reply))
                .await?;
            transfer(&mut socket.stream, &mut file, socket.size, progress).await?;
            file.sync_all().await.map_err(|e| e.to_string())?;
            drop(file);
            path.persist_noclobber(&destination)
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
    }

    pub async fn upload(
        &self,
        location: Location,
        source: PathBuf,
        progress: watch::Sender<Progress>,
    ) -> Result<(), String> {
        self.during_connection(async {
            if !tokio::fs::metadata(&source)
                .await
                .map_err(|e| e.to_string())?
                .is_file()
            {
                return Err("请选择普通文件".into());
            }
            let mut file = tokio::fs::File::open(source)
                .await
                .map_err(|e| e.to_string())?;
            let metadata = file.metadata().await.map_err(|e| e.to_string())?;
            if !metadata.is_file() {
                return Err("请选择普通文件".into());
            }
            let size = metadata.len();
            let mut socket = self
                .file_request(|reply| Command::Transfer(location, Some(size), reply))
                .await?;
            transfer(&mut file, &mut socket.stream, size, progress).await?;
            timeout(IO_TIMEOUT, socket.stream.shutdown())
                .await
                .map_err(|_| "等待服务器结束上传超时")?
                .map_err(|e| e.to_string())
        })
        .await
    }

    async fn during_connection<T>(
        &self,
        work: impl Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        let mut state = self.watch();
        let generation = state.borrow().generation;
        if state.borrow().status != ConnectionStatus::Connected {
            return Err("请先连接服务器".into());
        }
        tokio::select! {
            result = state.wait_for(|s| s.generation != generation || s.status != ConnectionStatus::Connected) => {
                let _ = result;
                Err("连接已中断，传输已停止".into())
            }
            result = work => result,
        }
    }
}

async fn transfer(
    source: &mut (impl AsyncRead + Unpin),
    destination: &mut (impl AsyncWrite + Unpin),
    total: u64,
    progress: watch::Sender<Progress>,
) -> Result<(), String> {
    let mut buffer = vec![0u8; 64 * 1024];
    let mut bytes = 0;
    let mut last_update = Instant::now();
    progress.send_replace(Progress { bytes, total });
    while bytes < total {
        let count = (total - bytes).min(buffer.len() as u64) as usize;
        let n = timeout(IO_TIMEOUT, source.read(&mut buffer[..count]))
            .await
            .map_err(|_| "文件读取超过 30 秒没有进展")?
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err(format!("文件提前结束，已传输 {bytes}/{total} 字节"));
        }
        timeout(IO_TIMEOUT, destination.write_all(&buffer[..n]))
            .await
            .map_err(|_| "文件写入超过 30 秒没有进展")?
            .map_err(|e| e.to_string())?;
        bytes += n as u64;
        if last_update.elapsed() >= Duration::from_millis(100) || bytes == total {
            last_update = Instant::now();
            progress.send_replace(Progress { bytes, total });
        }
    }
    timeout(IO_TIMEOUT, destination.flush())
        .await
        .map_err(|_| "文件写入超时")?
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_path_building_rejects_traversal_and_separators() {
        for bad in ["", ".", "..", "../outside", "a\\b", "a/b", "a\0b"] {
            assert!(child_path("/父目录", bad).is_err(), "{bad:?}");
        }
        assert_eq!(child_path("/", "文件.txt").unwrap(), "/文件.txt");
        assert_eq!(child_path("/父目录", "子目录").unwrap(), "/父目录/子目录");
        assert!(validate_path("/a/../secret", false).is_err());
        assert!(validate_path("/a//b", false).is_err());
    }

    #[tokio::test]
    async fn transfer_respects_advertised_size_and_reports_truncation() {
        let mut output = Vec::new();
        let (tx, rx) = watch::channel(Progress::default());
        let mut input = &b"abc trailing bytes"[..];
        transfer(&mut input, &mut output, 3, tx).await.unwrap();
        assert_eq!(output, b"abc");
        assert_eq!(rx.borrow().bytes, 3);
        let (tx, _) = watch::channel(Progress::default());
        assert!(
            transfer(&mut &b"abc"[..], &mut Vec::new(), 4, tx)
                .await
                .unwrap_err()
                .contains("提前结束")
        );
    }
}
