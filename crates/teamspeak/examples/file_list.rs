//! Read a directory in the configured test channel. Connection JSON comes from stdin.
use congspeak_core::{ConnectRequest, ConnectionStatus};
use congspeak_teamspeak::{IdentityMode, Session, files::Location};
use std::{io::Read, time::Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let request: ConnectRequest = serde_json::from_str(&input)?;
    let channel = request.channel.clone().ok_or("请指定测试频道 ID")?;
    let channel_password = request.channel_password.clone();
    let session = Session::start(IdentityMode::Ephemeral);
    let mut state = session.watch();
    session.connect(request).await?;
    let result = async {
        let connected = tokio::time::timeout(Duration::from_secs(35), async {
            let value = state
                .wait_for(|s| {
                    matches!(
                        s.status,
                        ConnectionStatus::Connected | ConnectionStatus::Failed
                    )
                })
                .await?;
            Ok::<_, Box<dyn std::error::Error>>(value.clone())
        })
        .await??;
        if connected.status != ConnectionStatus::Connected {
            return Err(connected.last_error.unwrap_or_default().into());
        }
        let entries = session
            .file_list(Location {
                generation: connected.generation,
                channel: channel.trim_start_matches('/').into(),
                path: "/".into(),
                password: channel_password,
            })
            .await?;
        println!(
            "PASS: server acknowledged directory list; {} entries",
            entries.len()
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = session.disconnect().await;
    result
}
