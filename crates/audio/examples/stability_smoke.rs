//! Real device worker regression. Capture gain is zero; outgoing bytes stay local.
//! Synthetic receive audio has drops/duplicates/bursts and a deliberately damaged Opus packet.
use audiopus::{Application, Channels, SampleRate, coder::Encoder};
use congspeak_audio::runtime::{AudioHandle, Control};
use congspeak_core::{AudioSettings, NoiseMode, TalkMode};
use std::{
    thread,
    time::{Duration, Instant},
};
use tsproto_packets::packets::{AudioData, CodecType, Direction, InAudioBuf, OutAudio};
fn packet(id: u16, bytes: &[u8]) -> InAudioBuf {
    let out = OutAudio::new(&AudioData::S2C {
        id,
        codec: CodecType::OpusVoice,
        from: 42,
        data: bytes,
    });
    InAudioBuf::try_new(Direction::S2C, out.into_vec()).unwrap()
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let settings = AudioSettings {
        input_muted: false,
        input_gain: 0.,
        talk_mode: TalkMode::Continuous,
        noise_mode: NoiseMode::Off,
        auto_gain: false,
        echo_cancellation: false,
        ..Default::default()
    };
    let (audio, mut outgoing) = AudioHandle::spawn(settings);
    audio.control(Control::Start)?;
    audio.control(Control::Permissions {
        send: true,
        receive: true,
        music: Some(false),
        quality: 7,
        epoch: 1,
    })?;
    for _ in 0..100 {
        if audio.status().input_ready && audio.status().output_ready {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    if !audio.status().input_ready || !audio.status().output_ready {
        audio.control(Control::Stop)?;
        return Err(format!("设备未就绪：{:?}", audio.status()).into());
    }
    let encoder = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip)?;
    let pcm: Vec<_> = (0..960)
        .map(|n| (n as f32 * 0.0576).sin() * 0.015)
        .collect();
    let mut bytes = [0; 1275];
    let len = encoder.encode_float(&pcm, &mut bytes)?;
    let seconds = std::env::args()
        .nth(1)
        .map(|v| v.parse::<u16>())
        .transpose()?
        .unwrap_or(4);
    let steps = seconds.checked_mul(50).ok_or("测试时长过大")?;
    let mut after_fault = 0;
    let mut after_restart = 0;
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        // Exact decoder failure sequence established by the headless regression.
        audio.receive(packet(0, &bytes[..len]));
        thread::sleep(Duration::from_millis(20));
        audio.receive(packet(1, &[131, 65]));
        thread::sleep(Duration::from_millis(120));
        while outgoing.try_recv().is_ok() {}
        for id in 20..20 + steps {
            if id % 13 != 0 {
                audio.receive(packet(id, &bytes[..len]));
            }
            if id % 7 == 0 {
                audio.receive(packet(id, &bytes[..len]));
            }
            if id % 17 == 0 {
                for old in id.saturating_sub(5)..id {
                    audio.receive(packet(old, &bytes[..len]));
                }
            }
            // A brief loss of session send permission must recover without reconfiguration.
            if id % 750 == 300 {
                audio.control(Control::Permissions {
                    send: false,
                    receive: true,
                    music: Some(false),
                    quality: 7,
                    epoch: 1,
                })?;
            }
            if id % 750 == 310 {
                audio.control(Control::Permissions {
                    send: true,
                    receive: true,
                    music: Some(false),
                    quality: 7,
                    epoch: 1,
                })?;
            }
            thread::sleep(Duration::from_millis(20));
            while let Ok(frame) = outgoing.try_recv() {
                if !frame.bytes.is_empty() {
                    after_fault += 1;
                }
            }
        }
        if after_fault < u64::from(steps) * 3 / 4 {
            return Err(format!(
                "接收故障后采集发送未持续：frames={after_fault}, {:?}",
                audio.status()
            )
            .into());
        }
        let status = audio.status();
        if status.playback_recoveries < 1 {
            return Err("测试未触发损坏解码队列的实际恢复".into());
        }
        audio.control(Control::Restart(2))?;
        audio.control(Control::Permissions {
            send: true,
            receive: true,
            music: Some(false),
            quality: 7,
            epoch: 2,
        })?;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(2) {
            while let Ok(frame) = outgoing.try_recv() {
                if frame.epoch == 2 && !frame.bytes.is_empty() {
                    after_restart += 1;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        if after_restart < 30 {
            return Err(format!("重启音频后未自动继续：frames={after_restart}").into());
        }
        if !audio.status().input_ready {
            return Err("采集设备意外停止".into());
        }
        println!(
            "PASS: real worker survived damaged/lost/duplicate/burst receive packets; capture encoded {after_fault} frames over {seconds}s afterward; explicit audio restart resumed {after_restart} frames; network never connected, capture gain zero."
        );
        Ok(())
    })();
    audio.control(Control::Stop)?;
    result
}
