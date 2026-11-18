use audiopus::{Application, Channels, SampleRate, coder::Encoder};
use congspeak_audio::runtime::{AudioHandle, Control};
use congspeak_core::AudioSettings;
use std::{thread, time::Duration};
use tsproto_packets::packets::{AudioData, CodecType, Direction, InAudioBuf, OutAudio};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (audio, mut outgoing) = AudioHandle::spawn(AudioSettings::default());
    audio.control(Control::Start)?;
    audio.control(Control::Permissions {
        send: false,
        receive: true,
        music: Some(false),
        quality: 7,
        epoch: 1,
    })?;
    for _ in 0..60 {
        if audio.status().output_ready {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    if !audio.status().output_ready {
        return Err(format!("无法打开输出设备：{:?}", audio.status().output_error).into());
    }
    let encoder = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip)?;
    let pcm: Vec<_> = (0..960)
        .map(|i| (i as f32 * 0.0576).sin() * 0.025)
        .collect();
    let mut bytes = [0; 1275];
    let n = encoder.encode_float(&pcm, &mut bytes)?;
    let mut peak = 0f32;
    for id in 0..40 {
        let packet = OutAudio::new(&AudioData::S2C {
            id,
            codec: CodecType::OpusVoice,
            from: 42,
            data: &bytes[..n],
        });
        audio.receive(InAudioBuf::try_new(Direction::S2C, packet.into_vec())?);
        thread::sleep(Duration::from_millis(20));
        peak = peak.max(audio.status().output_level);
    }
    let status = audio.status();
    audio.control(Control::Stop)?;
    assert!(!status.input_ready, "麦克风应保持关闭");
    assert!(outgoing.try_recv().is_err(), "不能发送网络音频");
    assert!(peak > 0.001, "播放链路未产生音频");
    println!(
        "PASS: real output stream opened; Opus receive/decode/mix/output RMS peak={peak:.5}; microphone stayed closed."
    );
    Ok(())
}
