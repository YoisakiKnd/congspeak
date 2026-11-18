use clap::Parser;
use congspeak_core::{ConnectRequest, ConnectionStatus};
use congspeak_teamspeak::{IdentityMode, Session};
use std::{io::Write, time::Duration};

#[derive(Parser)]
#[command(about = "CongSpeak TS3 protocol probe (microphone and playback disabled)")]
struct Args {
    #[arg(long)]
    address: String,
    #[arg(long, default_value = "CongSpeak-Probe")]
    nickname: String,
    #[arg(long)]
    channel: Option<String>,
    /// Read the server password from stdin; never put secrets in command arguments.
    #[arg(long)]
    password_stdin: bool,
    /// Use an explicitly temporary identity instead of the OS credential store.
    #[arg(long)]
    ephemeral: bool,
    /// Observe silently, then disconnect. Zero waits for Ctrl+C.
    #[arg(long, default_value_t = 10)]
    observe_seconds: u64,
    /// Emit one JSON snapshot after synchronization instead of names in the terminal.
    #[arg(long)]
    json: bool,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let args = Args::parse();
    let password = if args.password_stdin {
        let mut secret = String::new();
        std::io::stdin()
            .read_line(&mut secret)
            .map_err(|_| "无法读取密码")?;
        Some(secret.trim_end_matches(['\r', '\n']).to_owned())
    } else {
        None
    };
    let session = Session::start(if args.ephemeral {
        IdentityMode::Ephemeral
    } else {
        IdentityMode::SystemCredential
    });
    let mut state = session.watch();
    session
        .connect(ConnectRequest {
            address: args.address,
            nickname: args.nickname,
            password,
            channel: args.channel,
            channel_password: None,
        })
        .await?;
    let outcome = async {
        loop {
            let snapshot = state.borrow_and_update().clone();
            match snapshot.status {
                ConnectionStatus::Connected => {
                    if args.json {
                        println!(
                            "{}",
                            serde_json::to_string(&snapshot).map_err(|_| "无法编码快照")?
                        );
                    } else {
                        println!(
                            "Connected. {} channels, {} visible clients. Audio disabled.",
                            snapshot.channels.len(),
                            snapshot.clients.len()
                        );
                        for channel in &snapshot.channels {
                            // Escape control sequences in untrusted server-provided strings.
                            println!(
                                "channel {} {:?} ({})",
                                channel.id, channel.name, channel.codec
                            );
                        }
                        std::io::stdout().flush().map_err(|_| "输出失败")?;
                    }
                    if args.observe_seconds == 0 {
                        tokio::signal::ctrl_c().await.map_err(|_| "信号处理失败")?;
                    } else {
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_secs(args.observe_seconds)) => {},
                            _ = tokio::signal::ctrl_c() => {},
                            result = wait_for_failure(&mut state) => { result?; },
                        }
                    }
                    return Ok(());
                }
                ConnectionStatus::Failed => {
                    return Err(snapshot.last_error.unwrap_or_else(|| "连接失败".into()));
                }
                _ => {}
            }
            tokio::select! {
                result = state.changed() => result.map_err(|_| "会话任务已退出")?,
                _ = tokio::signal::ctrl_c() => return Ok(()),
            }
        }
    }
    .await;
    session.disconnect().await?;
    outcome
}

async fn wait_for_failure(
    state: &mut tokio::sync::watch::Receiver<congspeak_core::Snapshot>,
) -> Result<(), String> {
    loop {
        state.changed().await.map_err(|_| "会话任务已退出")?;
        let snapshot = state.borrow_and_update();
        if snapshot.status == ConnectionStatus::Failed {
            return Err(snapshot
                .last_error
                .clone()
                .unwrap_or_else(|| "连接失败".into()));
        }
    }
}
