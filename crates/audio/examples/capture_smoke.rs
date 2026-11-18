//! Local capture/DSP/Opus and release/mute checks. Never connects to a server or saves audio.
use congspeak_audio::runtime::{AudioHandle, Control};
use congspeak_core::AudioSettings;
use std::{
    thread,
    time::{Duration, Instant},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let settings = AudioSettings {
        input_muted: false,
        ..Default::default()
    };
    let (audio, mut outgoing) = AudioHandle::spawn(settings.clone());
    audio.control(Control::Start)?;
    audio.control(Control::Permissions {
        send: true,
        receive: false,
        music: Some(false),
        quality: 7,
        epoch: 1,
    })?;
    for _ in 0..100 {
        if audio.status().input_ready {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    if !audio.status().input_ready {
        audio.control(Control::Stop)?;
        return Err(format!("麦克风未就绪：{:?}", audio.status().input_error).into());
    }
    assert!(outgoing.try_recv().is_err(), "PTT 未按下不应编码发送");
    audio.control(Control::Ptt(true, 2))?;
    thread::sleep(Duration::from_millis(250));
    let mut frames = 0;
    while let Ok(frame) = outgoing.try_recv() {
        if frame.epoch == 2 && !frame.bytes.is_empty() {
            frames += 1;
        }
    }
    audio.control(Control::Ptt(false, 3))?;
    thread::sleep(Duration::from_millis(100));
    while outgoing.try_recv().is_ok() {}
    thread::sleep(Duration::from_millis(100));
    assert!(outgoing.try_recv().is_err(), "松开 PTT 后不得继续编码音频");
    audio.control(Control::Configure(
        AudioSettings {
            input_muted: true,
            ..settings
        },
        4,
    ))?;
    let start = Instant::now();
    while audio.status().input_ready && start.elapsed() < Duration::from_secs(2) {
        thread::sleep(Duration::from_millis(50));
    }
    assert!(!audio.status().input_ready, "静音后麦克风流应关闭");
    audio.control(Control::Stop)?;
    assert!(frames > 0, "未生成 Opus 音频帧");
    println!(
        "PASS: local microphone -> DSP -> Opus; {frames} frames; PTT release stopped encoding; mute closed capture. No network, no recordings saved."
    );
    Ok(())
}
