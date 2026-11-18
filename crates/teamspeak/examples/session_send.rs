//! Full capture -> Session -> TS3 regression. Input gain is zero: no environment audio leaves the machine.
use audiopus::{Channels, SampleRate, coder::Decoder};
use congspeak_core::{AudioSettings, ConnectRequest, ConnectionStatus, NoiseMode};
use congspeak_teamspeak::{IdentityMode, Session};
use futures::StreamExt;
use std::{
    io::Read,
    time::{Duration, Instant},
};
use tsclientlib::{Connection, DisconnectOptions, Identity, StreamItem};
use tsproto_packets::packets::AudioData;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut json = String::new();
    std::io::stdin().read_to_string(&mut json)?;
    let mut request: ConnectRequest = serde_json::from_str(&json)?;
    let channel = request.channel.clone().ok_or("必须指定授权测试频道")?;
    let settings = AudioSettings {
        input_gain: 0.,
        noise_mode: NoiseMode::Off,
        auto_gain: false,
        echo_cancellation: false,
        ..Default::default()
    };
    let session = Session::start_with_audio(IdentityMode::Ephemeral, settings.clone());
    request.nickname = "CongSpeak-Session-Silent-Send".into();
    session.connect(request.clone()).await?;
    let mut options = Connection::build(request.address)
        .name("CongSpeak-Silent-Receive")
        .channel(channel)
        .identity(Identity::create())
        .input_muted(true)
        .output_muted(false)
        .input_hardware_enabled(false)
        .output_hardware_enabled(true);
    if let Some(password) = request.password {
        options = options.password(password);
    }
    let mut receiver = options.connect()?;
    let result = tokio::time::timeout(Duration::from_secs(40), async {
        let mut decoder = Decoder::new(SampleRate::Hz48000, Channels::Mono)?;
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        let mut phase = 0;
        let mut since = Instant::now();
        let mut received = 0;
        let mut decoded_peak = 0f32;
        let mut sent_at_release = 0;
        loop {
            tokio::select! {
                event = next(&mut receiver) => {
                    if let StreamItem::Audio(packet) = event.ok_or("接收端断开")??
                        && let AudioData::S2C { from, data, .. } = packet.data().data()
                        && Some(*from) == session.snapshot().own_client_id && data.len() > 1 {
                        let mut pcm = [0.; 5760];
                        let n = decoder.decode_float(Some((*data).try_into()?), (&mut pcm[..]).try_into()?, false)?;
                        decoded_peak = decoded_peak.max(pcm[..n].iter().copied().map(f32::abs).fold(0f32, f32::max));
                        // Lossy Opus can reconstruct small comfort noise from zero input.
                        // Bound it to four 16-bit PCM steps, rather than requiring bit-exact silence.
                        if decoded_peak > 4. / 32768. {
                            return Err(format!("零增益语音包解码峰值过高：{decoded_peak}").into());
                        }
                        received += 1;
                    }
                },
                _ = tick.tick() => {
                    let snap = session.snapshot();
                    if snap.status == ConnectionStatus::Failed { return Err(snap.last_error.unwrap_or_default().into()); }
                    let ready = if let Ok(book) = receiver.get_state()
                        && let Some(own) = book.clients.get(&book.own_client)
                        && snap.own_client_id.is_some_and(|id| book.clients.contains_key(&tsclientlib::ClientId(id))) {
                        if book.clients.values().any(|c| c.channel == own.channel && c.id != book.own_client && Some(c.id.0) != snap.own_client_id) {
                            return Err("测试频道有其他用户，停止测试".into());
                        }
                        true
                    } else { false };
                    match phase {
                        0 if ready && snap.status == ConnectionStatus::Connected => {
                            session.set_audio_settings(AudioSettings {input_muted: false, ..settings.clone()}).await?;
                            phase = 1;
                        },
                        1 if snap.audio.input_ready && snap.audio.can_send => {since = Instant::now(); phase = 2;},
                        2 if since.elapsed() > Duration::from_millis(350) => {
                            if snap.audio.sent_packets != 0 || received != 0 { return Err("未按 PTT 已发送语音".into()); }
                            session.native_ptt(true); since = Instant::now(); phase = 3;
                        },
                        3 if since.elapsed() > Duration::from_millis(800) => {
                            session.native_ptt(false); since = Instant::now(); phase = 4;
                        },
                        4 if since.elapsed() > Duration::from_millis(250) => {
                            sent_at_release = snap.audio.sent_packets; since = Instant::now(); phase = 5;
                        },
                        5 if since.elapsed() > Duration::from_millis(300) => {
                            if sent_at_release < 10 || received < 10 { return Err(format!("Session 未有效发送：sent={sent_at_release}, received={received}, status={:?}", snap.audio).into()); }
                            if snap.audio.sent_packets != sent_at_release { return Err("释放 PTT 后继续发送语音".into()); }
                            session.set_audio_settings(settings.clone()).await?; since = Instant::now(); phase = 6;
                        },
                        6 if !snap.audio.input_ready => {
                            println!("PASS: capture -> DSP (zero gain) -> Opus -> Session -> TS3 -> peer decode; sent={sent_at_release}, received={received}, decoded_peak={decoded_peak:.7}; PTT idle/release and mute verified; no environment audio transmitted.");
                            return Ok::<(), Box<dyn std::error::Error>>(());
                        },
                        _ => {}
                    }
                }
            }
        }
    }).await;
    let _ = session.set_audio_settings(settings).await;
    let _ = session.disconnect().await;
    let _ = receiver.disconnect(DisconnectOptions::new());
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(event) = receiver.events().next().await {
            if event.is_err() {
                break;
            }
        }
    })
    .await;
    result??;
    Ok(())
}
async fn next(c: &mut Connection) -> Option<Result<StreamItem, tsclientlib::Error>> {
    c.events().next().await
}
