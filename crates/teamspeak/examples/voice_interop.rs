//! Two temporary peers exchange synthetic audio only in an otherwise empty authorized channel.
use audiopus::{
    Application, Channels, SampleRate,
    coder::{Decoder, Encoder},
};
use futures::StreamExt;
use std::{
    io::Read,
    time::{Duration, Instant},
};
use tsclientlib::{Connection, DisconnectOptions, Identity, StreamItem};
use tsproto_packets::packets::{AudioData, CodecType, OutAudio};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut json = String::new();
    std::io::stdin().read_to_string(&mut json)?;
    let request: congspeak_core::ConnectRequest = serde_json::from_str(&json)?;
    let channel = request.channel.as_ref().ok_or("需要明确指定测试频道")?;
    let options = |name: &str| {
        let mut options = Connection::build(request.address.clone())
            .name(name.to_owned())
            .channel(channel.clone())
            .identity(Identity::create())
            .input_muted(false)
            .output_muted(false)
            .input_hardware_enabled(true)
            .output_hardware_enabled(true);
        if let Some(password) = &request.password {
            options = options.password(password.clone());
        }
        options
    };
    let mut a = options("CongSpeak-Voice-Receive").connect()?;
    let mut b = options("CongSpeak-Voice-Send").connect()?;
    let result=tokio::time::timeout(Duration::from_secs(35),async{
        let encoder=Encoder::new(SampleRate::Hz48000,Channels::Mono,Application::Voip)?;
        let mut decoder=Decoder::new(SampleRate::Hz48000,Channels::Mono)?;
        let pcm:Vec<_>=(0..960).map(|i|(i as f32*std::f32::consts::TAU*440./48000.).sin()*0.025).collect();
        let mut data=[0u8;1275];let length=encoder.encode_float(&pcm,&mut data)?;
        let mut tick=tokio::time::interval(Duration::from_millis(20));
        let mut started:Option<Instant>=None;let mut sent=0;let mut received=0;let mut power=0f64;let mut samples=0;
        loop {
            tokio::select!{
                event=next(&mut a)=>{
                    if let StreamItem::Audio(packet)=event.ok_or("接收端已断开")??
                            && let AudioData::S2C{from,data,..}=packet.data().data()
                                && Some(*from)==b.get_state().ok().map(|book|book.own_client.0)
                                && data.len()>1 {
                                let mut decoded=[0f32;5760];let n=decoder.decode_float(Some((*data).try_into()?),(&mut decoded[..]).try_into()?,false)?;
                                power+=decoded[..n].iter().map(|s|f64::from(*s)*f64::from(*s)).sum::<f64>();samples+=n;received+=1;
                    }
                },
                event=next(&mut b)=>{event.ok_or("发送端已断开")??;},
                _=tick.tick()=>{
                    if started.is_none(){
                        if let (Ok(ab),Ok(bb))=(a.get_state(),b.get_state())
                            && let (Some(ac),Some(bc))=(ab.clients.get(&ab.own_client),bb.clients.get(&bb.own_client))
                            && ac.channel==bc.channel && ab.clients.contains_key(&bb.own_client) {
                            if ab.clients.values().any(|c|c.channel==ac.channel&&c.id!=ab.own_client&&c.id!=bb.own_client){return Err("测试频道有其他用户，未发送测试音".into());}
                            started=Some(Instant::now());
                        }
                    }else if sent<25{
                        b.send_audio(OutAudio::new(&AudioData::C2S{id:0,codec:CodecType::OpusVoice,data:&data[..length]}))?;sent+=1;
                    }else if sent==25 {b.send_audio(OutAudio::new(&AudioData::C2S{id:0,codec:CodecType::OpusVoice,data:&[]}))?;sent+=1;}
                    else if started.is_some_and(|time|time.elapsed()>Duration::from_secs(2)){
                        let rms=(power/samples.max(1) as f64).sqrt();
                        if received<10||rms<0.001{return Err(format!("语音回环未通过：received={received}, rms={rms}").into());}
                        println!("PASS: received={received} Opus packets, decoded_rms={rms:.5}; synthetic audio only.");
                        return Ok::<(),Box<dyn std::error::Error>>(());
                    }
                }
            }
        }
    }).await;
    for con in [&mut a, &mut b] {
        let _ = con.disconnect(DisconnectOptions::new());
    }
    let _ = tokio::join!(
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(e) = a.events().next().await {
                if e.is_err() {
                    break;
                }
            }
        }),
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(e) = b.events().next().await {
                if e.is_err() {
                    break;
                }
            }
        })
    );
    result??;
    Ok(())
}

async fn next(c: &mut Connection) -> Option<Result<StreamItem, tsclientlib::Error>> {
    c.events().next().await
}
