use congspeak_audio::{FRAME_SAMPLES, RnNoise};
use cpal::traits::{DeviceTrait, HostTrait};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|argument| argument == "--devices") {
        let host = cpal::default_host();
        println!("Audio host: {:?}", host.id());
        for device in host.devices()? {
            println!("Device: {:?}", device.description()?);
        }
        return Ok(());
    }
    // Synthetic PCM only. This probe does not open microphone/playback streams.
    let mut denoiser = RnNoise::default();
    #[cfg(feature = "webrtc")]
    let apm = congspeak_audio::WebRtc::new(true, true, true)?;
    let mut times = Vec::with_capacity(1000);
    let mut seed = 7_u32;
    for _ in 0..1000 {
        let mut frame = [0.; FRAME_SAMPLES];
        for sample in &mut frame {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *sample = (seed as f64 / u32::MAX as f64 * 0.02 - 0.01) as f32;
        }
        let start = std::time::Instant::now();
        let mut rnnoise_frame = frame;
        denoiser.process(&mut rnnoise_frame);
        #[cfg(feature = "webrtc")]
        {
            apm.render(&[0.; FRAME_SAMPLES])?;
            apm.capture(&mut frame)?;
        }
        if !frame
            .iter()
            .chain(rnnoise_frame.iter())
            .all(|sample| sample.is_finite())
        {
            return Err("non-finite DSP output".into());
        }
        times.push(start.elapsed().as_micros());
    }
    times.sort_unstable();
    println!("Synthetic 48 kHz / 10 ms PCM: 1000 frames processed.");
    println!(
        "RNNoise enabled; WebRTC NS/AEC/AGC enabled: {}",
        cfg!(feature = "webrtc")
    );
    println!(
        "Combined processing: p50={} µs, p99={} µs (synthetic smoke test, not voice quality validation).",
        times[500], times[990]
    );
    Ok(())
}
