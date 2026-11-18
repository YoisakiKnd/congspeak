//! Exercise the Session against an authorized server through a local UDP relay.
//! The JSON on stdin uses ConnectRequest fields. Audio and chat stay disabled.
use congspeak_core::{ClientAction, ConnectRequest, ConnectionStatus, Snapshot};
use congspeak_teamspeak::{IdentityMode, Session};
use std::{io::Read, net::SocketAddr, time::Duration};
use tokio::{
    net::{UdpSocket, lookup_host},
    sync::{oneshot, watch},
    time::timeout,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut json = String::new();
    std::io::stdin().read_to_string(&mut json)?;
    let mut request: ConnectRequest = serde_json::from_str(&json)?;
    if request.channel.is_none() {
        return Err("指定授权的测试频道".into());
    }
    let address = if request.address.parse::<SocketAddr>().is_ok() || request.address.contains(':')
    {
        request.address.clone()
    } else {
        format!("{}:9987", request.address)
    };
    let server = lookup_host(address)
        .await?
        .find(SocketAddr::is_ipv4)
        .ok_or("测试中继需 IPv4 服务器")?;
    let client_socket = UdpSocket::bind("127.0.0.1:0").await?;
    let server_socket = UdpSocket::bind("0.0.0.0:0").await?;
    server_socket.connect(server).await?;
    request.address = client_socket.local_addr()?.to_string();
    let (outage, loss) = watch::channel(false);
    let (worker_finished, finished) = oneshot::channel();
    let relay = tokio::spawn(async move {
        let result = relay(client_socket, server_socket, loss).await;
        let _ = worker_finished.send(result);
    });

    let session = Session::start(IdentityMode::Ephemeral);
    let mut state = session.watch();
    let result = tokio::select! {
        result = async {
            session.connect(request).await?;
            let first = wait_for(&mut state, |s| s.status == ConnectionStatus::Connected).await?;
            session.action_confirmed(first.generation, None, ClientAction::Subscribe { enabled: true }).await?;
            outage.send_replace(true);
            let retrying = wait_for(&mut state, |s| s.status == ConnectionStatus::Reconnecting).await?;
            if retrying.generation == first.generation || !retrying.clients.is_empty() {
                return Err("重连未及时作废旧客户端状态".into());
            }
            outage.send_replace(false);
            let after = wait_for(&mut state, |s| s.status == ConnectionStatus::Connected).await?;
            if after.generation == first.generation {
                return Err("重连未更新连接代数".into());
            }
            let stale = session.action_confirmed(first.generation, None, ClientAction::Subscribe { enabled: true }).await;
            if !stale.is_err_and(|e| e.contains("连接已变化")) {
                return Err("旧连接命令未被拒绝".into());
            }
            session.action_confirmed(after.generation, None, ClientAction::Subscribe { enabled: true }).await?;
            println!("PASS: UDP outage, new session generation, stale request rejected, fresh command acknowledged");
            Ok::<(), Box<dyn std::error::Error>>(())
        } => result,
        result = finished => Err(format!("UDP 中继退出：{result:?}").into()),
    };
    let _ = session.disconnect().await;
    relay.abort();
    result
}

async fn wait_for(
    state: &mut watch::Receiver<Snapshot>,
    predicate: impl Fn(&Snapshot) -> bool,
) -> Result<Snapshot, String> {
    timeout(Duration::from_secs(48), async {
        loop {
            let snapshot = state.borrow_and_update().clone();
            if predicate(&snapshot) {
                return Ok(snapshot);
            }
            if snapshot.status == ConnectionStatus::Failed {
                return Err(snapshot.last_error.unwrap_or("连接失败".into()));
            }
            state.changed().await.map_err(|e| e.to_string())?;
        }
    })
    .await
    .map_err(|_| "等待连接状态超时")?
}

async fn relay(
    local: UdpSocket,
    upstream: UdpSocket,
    loss: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let mut sender = None;
    let mut client_bytes = [0; 4096];
    let mut server_bytes = [0; 4096];
    loop {
        tokio::select! {
            packet = local.recv_from(&mut client_bytes) => {
                let (length, from) = packet?;
                if sender != Some(from) {
                    sender = Some(from);
                }
                if !*loss.borrow() {
                    upstream.send(&client_bytes[..length]).await?;
                }
            }
            packet = upstream.recv(&mut server_bytes) => {
                let length = packet?;
                if !*loss.borrow() && let Some(to) = sender {
                    local.send_to(&server_bytes[..length], to).await?;
                }
            }
        }
    }
}
