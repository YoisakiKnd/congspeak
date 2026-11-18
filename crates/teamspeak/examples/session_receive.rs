//! Verifies the actual Session -> jitter buffer -> CPAL playback path without opening a microphone.
use audiopus::{Application, Channels, SampleRate, coder::Encoder};
use congspeak_core::{AudioSettings, ConnectRequest, ConnectionStatus};
use congspeak_teamspeak::{IdentityMode, Session};
use futures::StreamExt;
use std::{
    io::Read,
    time::{Duration, Instant},
};
use tsclientlib::{Connection, DisconnectOptions, Identity, StreamItem};
use tsproto_packets::packets::{AudioData, CodecType, OutAudio};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let mut request: ConnectRequest = serde_json::from_str(&input)?;
    let channel = request.channel.clone().ok_or("必须指定测试频道")?;
    let session = Session::start_with_audio(IdentityMode::Ephemeral, AudioSettings::default());
    request.nickname = "CongSpeak-Session-Receive".into();
    session.connect(request.clone()).await?;
    let mut options = Connection::build(request.address)
        .name("CongSpeak-Synthetic-Send")
        .identity(Identity::create())
        .channel(channel)
        .input_muted(false)
        .output_muted(false)
        .input_hardware_enabled(true)
        .output_hardware_enabled(true);
    if let Some(password) = request.password {
        options = options.password(password);
    }
    let mut sender = options.connect()?;
    let result=tokio::time::timeout(Duration::from_secs(35),async{
        let encoder=Encoder::new(SampleRate::Hz48000,Channels::Mono,Application::Voip)?;
        let pcm:Vec<_>=(0..960).map(|i|(i as f32*0.0576).sin()*0.025).collect();let mut bytes=[0;1275];let length=encoder.encode_float(&pcm,&mut bytes)?;
        let mut tick=tokio::time::interval(Duration::from_millis(20));let mut sent=0;let mut peak=0f32;let mut started:Option<Instant>=None;
        loop{tokio::select!{
            event=next(&mut sender)=>{event.ok_or("发送端断开")??;},
            _=tick.tick()=>{
                let snapshot=session.snapshot();peak=peak.max(snapshot.audio.output_level);
                if snapshot.status==ConnectionStatus::Failed{return Err(snapshot.last_error.unwrap_or_default().into());}
                if started.is_none(){
                    if snapshot.status==ConnectionStatus::Connected && snapshot.audio.output_ready
                        && let Ok(book)=sender.get_state() && let Some(own)=book.clients.get(&book.own_client)
                        && snapshot.own_client_id.is_some_and(|id|book.clients.keys().any(|client|client.0==id)){
                        if book.clients.values().any(|c|c.channel==own.channel&&c.id!=book.own_client&&Some(c.id.0)!=snapshot.own_client_id){return Err("频道有其他用户，未发送测试音".into());}
                        started=Some(Instant::now());
                    }
                }else if sent<35{sender.send_audio(OutAudio::new(&AudioData::C2S{id:0,codec:CodecType::OpusVoice,data:&bytes[..length]}))?;sent+=1;}
                else if sent==35{sender.send_audio(OutAudio::new(&AudioData::C2S{id:0,codec:CodecType::OpusVoice,data:&[]}))?;sent+=1;}
                else if started.is_some_and(|time|time.elapsed()>Duration::from_secs(2)){
                    if peak<0.001{return Err(format!("实际 Session 播放无输出：peak={peak}, status={:?}",snapshot.audio).into());}
                    if snapshot.audio.input_ready{return Err("测试不应打开麦克风".into());}
                    println!("PASS: TS3 -> Session -> Opus decode/mix -> real CPAL output; peak_rms={peak:.5}; microphone closed.");return Ok::<(),Box<dyn std::error::Error>>(());
                }
            }
        }}
    }).await;
    let _ = sender.disconnect(DisconnectOptions::new());
    let _ = session.disconnect().await;
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(e) = sender.events().next().await {
            if e.is_err() {
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
