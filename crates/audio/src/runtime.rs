use crate::playback::Playback;
use crate::{FRAME_SAMPLES, Frame, RnNoise};
use audiopus::{Application, Bitrate, Channels, SampleRate, Signal, coder::Encoder};
use congspeak_core::{AudioDevice, AudioSettings, AudioStatus, NoiseMode, TalkMode};
use cpal::{
    FromSample, Sample, SampleFormat, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};
use sonora_agc2::vad_wrapper::VoiceActivityDetectorWrapper;
use std::{
    collections::{HashSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tsproto_packets::packets::{AudioData, InAudioBuf};

pub struct EncodedAudio {
    pub epoch: u64,
    pub created: Instant,
    pub bytes: Vec<u8>,
    pub music: bool,
}
pub enum Control {
    Start,
    Stop,
    Configure(AudioSettings, u64),
    Permissions {
        send: bool,
        receive: bool,
        music: Option<bool>,
        quality: u8,
        epoch: u64,
    },
    Ptt(bool, u64),
    MicTest(bool, u64),
    TestSpeaker,
    PresenceTone(bool),
    Restart(u64),
    UserVolume(u16, f32),
    IgnoreClients(HashSet<u16>),
    ClearPeers,
    RemovePeer(u16),
}
enum Work {
    Control(Control),
    Packet(InAudioBuf),
}
#[derive(Clone)]
pub struct AudioHandle {
    tx: mpsc::SyncSender<Work>,
    controls: mpsc::Sender<Control>,
    state: Arc<Mutex<AudioStatus>>,
    muted: Arc<AtomicBool>,
    inbound_drops: Arc<AtomicU64>,
}
impl AudioHandle {
    pub fn spawn(settings: AudioSettings) -> (Self, tokio::sync::mpsc::Receiver<EncodedAudio>) {
        let (tx, rx) = mpsc::sync_channel(128);
        let (controls, control_rx) = mpsc::channel();
        let (out, outgoing) = tokio::sync::mpsc::channel(8);
        let state = Arc::new(Mutex::new(AudioStatus::default()));
        let muted = Arc::new(AtomicBool::new(settings.output_muted));
        let handle = Self {
            tx,
            controls,
            state: state.clone(),
            muted: muted.clone(),
            inbound_drops: Arc::new(AtomicU64::new(0)),
        };
        thread::Builder::new()
            .name("congspeak-audio".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    worker(rx, control_rx, out, settings, state.clone(), muted)
                }));
                if result.is_err() {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    *s = AudioStatus {
                        processing_error: Some("音频线程异常停止，请重新启动应用".into()),
                        ..Default::default()
                    };
                }
            })
            .expect("audio worker thread");
        (handle, outgoing)
    }
    pub fn control(&self, command: Control) -> Result<(), String> {
        if let Control::Configure(settings, _) = &command {
            self.muted.store(settings.output_muted, Ordering::Relaxed);
        }
        self.controls
            .send(command)
            .map_err(|_| "音频任务忙或已退出，请重试".into())
    }
    pub fn receive(&self, packet: InAudioBuf) {
        if let Err(mpsc::TrySendError::Full(_)) = self.tx.try_send(Work::Packet(packet)) {
            self.inbound_drops.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn status(&self) -> AudioStatus {
        let mut status = self.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
        status.discarded_packets += self.inbound_drops.load(Ordering::Relaxed);
        status
    }
}

pub fn devices() -> Result<Vec<AudioDevice>, String> {
    let host = cpal::default_host();
    host.devices()
        .map_err(|e| e.to_string())?
        .map(|d| {
            let description = d.description().map_err(|e| e.to_string())?;
            Ok(AudioDevice {
                id: d.id().map_err(|e| e.to_string())?.to_string(),
                name: description.name().to_string(),
                // WASAPI also exposes loopback capture configs on render endpoints.
                // Those are not microphones and must not appear in the input picker.
                input: d.supports_input(),
                output: d.supports_output(),
            })
        })
        .collect()
}

fn device(id: &Option<String>, input: bool) -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    if let Some(id) = id {
        let parsed = id.parse().map_err(|_| "保存的设备标识无效")?;
        let selected = host
            .device_by_id(&parsed)
            .ok_or("选择的设备不可用，请重新选择或刷新设备")?;
        if (input && !selected.supports_input()) || (!input && !selected.supports_output()) {
            return Err("保存的设备方向不匹配，请在音频设置中重新选择麦克风或扬声器".into());
        }
        return Ok(selected);
    }
    (if input {
        host.default_input_device()
    } else {
        host.default_output_device()
    })
    .ok_or_else(|| "未找到系统默认音频设备".into())
}
fn config(d: &cpal::Device, input: bool) -> Result<cpal::SupportedStreamConfig, String> {
    let configs: Vec<_> = if input {
        d.supported_input_configs()
            .map_err(|e| e.to_string())?
            .collect()
    } else {
        d.supported_output_configs()
            .map_err(|e| e.to_string())?
            .collect()
    };
    if let Some(c) = configs
        .into_iter()
        .filter(|c| c.channels() <= 2 && c.sample_format() == SampleFormat::F32)
        .find_map(|c| c.try_with_sample_rate(48_000))
    {
        return Ok(c);
    }
    (if input {
        d.default_input_config()
    } else {
        d.default_output_config()
    })
    .map_err(|e| e.to_string())
}
fn stream_needs_restart(kind: cpal::ErrorKind) -> bool {
    // These notifications leave the stream alive. Restarting after every WASAPI
    // discontinuity can otherwise prevent capture from ever becoming usable.
    !matches!(
        kind,
        cpal::ErrorKind::Xrun | cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied
    )
}
fn input_stream<T>(
    d: &cpal::Device,
    c: &cpal::StreamConfig,
    mut prod: HeapProd<f32>,
    health: Arc<StreamHealth>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(c.channels);
    let progress = health.clone();
    d.build_input_stream(
        *c,
        move |data: &[T], info| {
            if !data.is_empty() {
                let stamp = info.timestamp();
                progress
                    .delay_ms
                    .store(delay_ms(stamp.callback, stamp.capture), Ordering::Relaxed);
                progress.callbacks.fetch_add(1, Ordering::Relaxed);
            }
            let mut lost = 0;
            let mut clipped = 0;
            for frame in data.chunks_exact(channels) {
                let value =
                    frame.iter().map(|s| f32::from_sample(*s)).sum::<f32>() / channels as f32;
                clipped += u64::from(value.abs() >= 0.999);
                if prod
                    .try_push(if value.is_finite() { value } else { 0. })
                    .is_err()
                {
                    lost += 1;
                }
            }
            if clipped > 0 {
                progress
                    .clipped_samples
                    .fetch_add(clipped, Ordering::Relaxed);
            }
            if lost > 0 {
                progress.dropped_samples.fetch_add(lost, Ordering::Relaxed);
            }
        },
        move |error| {
            if stream_needs_restart(error.kind()) {
                health.failed.store(true, Ordering::Relaxed);
            }
        },
        None,
    )
    .map_err(|e| e.to_string())
}
fn output_stream<T>(
    d: &cpal::Device,
    c: &cpal::StreamConfig,
    mut cons: HeapCons<f32>,
    mut reference: HeapProd<f32>,
    health: Arc<StreamHealth>,
    muted: Arc<AtomicBool>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(c.channels);
    let progress = health.clone();
    d.build_output_stream(
        *c,
        move |data: &mut [T], info| {
            if !data.is_empty() {
                let stamp = info.timestamp();
                progress
                    .delay_ms
                    .store(delay_ms(stamp.playback, stamp.callback), Ordering::Relaxed);
                progress.callbacks.fetch_add(1, Ordering::Relaxed);
            }
            for frame in data.chunks_mut(channels) {
                let left = cons.try_pop().unwrap_or(0.);
                let right = cons.try_pop().unwrap_or(0.);
                let (left, right) = if muted.load(Ordering::Relaxed) {
                    (0., 0.)
                } else {
                    (left, right)
                };
                let mono = (left + right) * 0.5;
                for (channel, sample) in frame.iter_mut().enumerate() {
                    *sample = T::from_sample(if channels == 1 {
                        mono
                    } else {
                        match channel {
                            0 => left,
                            1 => right,
                            _ => 0.,
                        }
                    });
                }
                let _ = reference.try_push(mono);
            }
        },
        move |error| {
            if stream_needs_restart(error.kind()) {
                health.failed.store(true, Ordering::Relaxed);
            }
        },
        None,
    )
    .map_err(|e| e.to_string())
}
macro_rules! format_stream {
    ($format:expr, $function:ident, $($arg:expr),*) => { match $format {
        SampleFormat::F32 => $function::<f32>($($arg),*), SampleFormat::F64 => $function::<f64>($($arg),*),
        SampleFormat::I8 => $function::<i8>($($arg),*), SampleFormat::I16 => $function::<i16>($($arg),*),
        SampleFormat::I32 => $function::<i32>($($arg),*), SampleFormat::I64 => $function::<i64>($($arg),*),
        SampleFormat::U8 => $function::<u8>($($arg),*), SampleFormat::U16 => $function::<u16>($($arg),*),
        SampleFormat::U32 => $function::<u32>($($arg),*), SampleFormat::U64 => $function::<u64>($($arg),*),
        _ => Err("设备采样格式暂不支持".into())
    }};
}
fn delay_ms(later: cpal::StreamInstant, earlier: cpal::StreamInstant) -> u64 {
    later
        .checked_duration_since(earlier)
        .map_or(0, |d| d.as_millis().min(500) as u64)
}

#[derive(Default)]
struct StreamHealth {
    failed: AtomicBool,
    callbacks: AtomicU64,
    delay_ms: AtomicU64,
    clipped_samples: AtomicU64,
    dropped_samples: AtomicU64,
}
struct CallbackWatch {
    last: u64,
    changed_at: Instant,
}
impl CallbackWatch {
    fn new(now: Instant) -> Self {
        Self {
            last: 0,
            changed_at: now,
        }
    }
    fn stalled(&mut self, callbacks: u64, now: Instant) -> bool {
        if callbacks != self.last {
            self.last = callbacks;
            self.changed_at = now;
        }
        now.duration_since(self.changed_at) > Duration::from_secs(2)
    }
}
struct Input {
    id: String,
    _stream: cpal::Stream,
    samples: HeapCons<f32>,
    rate: u32,
    health: Arc<StreamHealth>,
    watchdog: CallbackWatch,
}
struct Output {
    id: String,
    _stream: cpal::Stream,
    samples: HeapProd<f32>,
    reference: HeapCons<f32>,
    rate: u32,
    health: Arc<StreamHealth>,
    watchdog: CallbackWatch,
}
fn open_input(settings: &AudioSettings) -> Result<Input, String> {
    let d = device(&settings.input_device, true)?;
    let c = config(&d, true)?;
    let (prod, cons) = HeapRb::new(c.sample_rate() as usize / 5).split();
    let health = Arc::new(StreamHealth::default());
    let stream = format_stream!(
        c.sample_format(),
        input_stream,
        &d,
        &c.config(),
        prod,
        health.clone()
    )?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Input {
        id: d.id().map_err(|e| e.to_string())?.to_string(),
        _stream: stream,
        samples: cons,
        rate: c.sample_rate(),
        health,
        watchdog: CallbackWatch::new(Instant::now()),
    })
}
fn open_output(settings: &AudioSettings, muted: Arc<AtomicBool>) -> Result<Output, String> {
    let d = device(&settings.output_device, false)?;
    let c = config(&d, false)?;
    let (prod, cons) = HeapRb::new(c.sample_rate() as usize / 5 * 2).split();
    let (reference, reference_cons) = HeapRb::new(c.sample_rate() as usize / 5).split();
    let health = Arc::new(StreamHealth::default());
    let stream = format_stream!(
        c.sample_format(),
        output_stream,
        &d,
        &c.config(),
        cons,
        reference,
        health.clone(),
        muted
    )?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Output {
        id: d.id().map_err(|e| e.to_string())?.to_string(),
        _stream: stream,
        samples: prod,
        reference: reference_cons,
        rate: c.sample_rate(),
        health,
        watchdog: CallbackWatch::new(Instant::now()),
    })
}

/// Continuous linear interpolation with a fractional cursor shared across callbacks.
/// The worker owns it; device callbacks remain allocation-free and nonblocking.
#[derive(Default)]
pub struct Resampler {
    previous: Option<f32>,
    position: f64,
}
impl Resampler {
    pub fn push(&mut self, sample: f32, from: u32, to: u32, mut emit: impl FnMut(f32)) {
        if let Some(previous) = self.previous {
            while self.position < 1. {
                emit(previous + (sample - previous) * self.position as f32);
                self.position += from as f64 / to as f64;
            }
            self.position -= 1.;
        }
        self.previous = Some(sample);
    }
}

pub struct Processor {
    post_gain: Option<sonora::AudioProcessing>,
    apm: sonora::AudioProcessing,
    rnnoise: Option<RnNoise>,
    vad: Option<VoiceActivityDetectorWrapper>,
    input_gain: f32,
    settings: AudioSettings,
}
impl Processor {
    pub fn new(settings: &AudioSettings, preview: bool) -> Self {
        use sonora::config::*;
        let config = sonora::Config {
            noise_suppression: (settings.noise_mode == NoiseMode::Standard)
                .then(NoiseSuppression::default),
            echo_canceller: (settings.echo_cancellation && !preview).then(EchoCanceller::default),
            gain_controller2: (settings.auto_gain && settings.noise_mode != NoiseMode::Rnnoise)
                .then(|| GainController2 {
                    adaptive_digital: Some(AdaptiveDigital {
                        initial_gain_db: 0.,
                        max_gain_db: 20.,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            high_pass_filter: Some(HighPassFilter::default()),
            ..Default::default()
        };
        let apm = sonora::AudioProcessing::builder()
            .config(config)
            .capture_config(sonora::StreamConfig::new(48_000, 1))
            .render_config(sonora::StreamConfig::new(48_000, 1))
            .build();
        let post_gain = if settings.auto_gain && settings.noise_mode == NoiseMode::Rnnoise {
            Some(
                sonora::AudioProcessing::builder()
                    .config(sonora::Config {
                        gain_controller2: Some(GainController2 {
                            adaptive_digital: Some(AdaptiveDigital {
                                initial_gain_db: 0.,
                                max_gain_db: 20.,
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })
                    .capture_config(sonora::StreamConfig::new(48000, 1))
                    .render_config(sonora::StreamConfig::new(48000, 1))
                    .build(),
            )
        } else {
            None
        };
        Self {
            apm,
            post_gain,
            rnnoise: (settings.noise_mode == NoiseMode::Rnnoise).then(RnNoise::default),
            vad: (settings.talk_mode == TalkMode::VoiceActivation)
                .then(|| VoiceActivityDetectorWrapper::new(sonora_simd::detect_backend(), 48_000)),
            input_gain: settings.input_gain,
            settings: settings.clone(),
        }
    }
    pub fn render(&mut self, frame: &Frame) -> Result<(), String> {
        if frame.iter().any(|s| !s.is_finite()) {
            return Err("播放参考信号包含无效采样".into());
        }
        let mut output = [0.; FRAME_SAMPLES];
        self.apm
            .process_render_f32(&[frame], &mut [&mut output])
            .map_err(|e| e.to_string())
    }
    fn set_echo_delay(&mut self, millis: u16) {
        let delay = i32::from(millis);
        if self.apm.stream_delay_ms() != delay {
            let _ = self.apm.set_stream_delay_ms(delay);
        }
    }

    pub fn capture(&mut self, frame: &mut Frame) -> Result<Option<f32>, String> {
        if frame.iter().any(|s| !s.is_finite()) {
            return Err("麦克风处理信号包含无效采样".into());
        }
        let mut source = *frame;
        let peak = source.iter().map(|s| s.abs()).fold(0., f32::max);
        let target = self
            .settings
            .input_gain
            .min(0.97 / peak.max(f32::MIN_POSITIVE));
        // Lower the gain before an over-range frame can clip. Recover over several
        // frames to avoid modulating each waveform peak independently.
        if target < self.input_gain {
            self.input_gain = target;
            for sample in &mut source {
                *sample *= target;
            }
        } else {
            let start = self.input_gain;
            self.input_gain += (target - start) * 0.04;
            let step = (self.input_gain - start) / FRAME_SAMPLES as f32;
            for (index, sample) in source.iter_mut().enumerate() {
                *sample *= start + (index + 1) as f32 * step;
            }
        }
        // RNNoise runs after AEC and before AGC; APM NS is disabled in this mode.
        self.apm
            .process_capture_f32(&[&source], &mut [frame.as_mut_slice()])
            .map_err(|e| e.to_string())?;
        if let Some(denoiser) = &mut self.rnnoise {
            denoiser.process(frame);
        }
        let voice_probability = self.vad.as_mut().map(|vad| {
            let analysis = frame.map(|sample| sample * 32768.);
            let probability = vad.analyze(&analysis);
            // Broadband noise has substantial sample-to-sample variation relative to
            // its energy. Compare the shape, so a louder copy makes the same decision.
            let energy: f32 = source.iter().map(|sample| sample * sample).sum();
            let change: f32 = source
                .windows(2)
                .map(|pair| (pair[1] - pair[0]).powi(2))
                .sum();
            if change > 1.25 * energy {
                0.
            } else {
                probability
            }
        });
        if let Some(gain) = &mut self.post_gain {
            let source = *frame;
            gain.process_capture_f32(&[&source], &mut [frame.as_mut_slice()])
                .map_err(|e| e.to_string())?;
        }
        if frame.iter().any(|s| !s.is_finite()) || voice_probability.is_some_and(|p| !p.is_finite())
        {
            return Err("声音处理产生无效采样".into());
        }
        Ok(voice_probability)
    }
}

/// Rebuild a failed DSP graph without muting settings or disconnecting the session.
/// Drop frames during a short retry backoff; never bypass the user's processing settings.
struct RecoveringProcessor {
    inner: Option<Box<Processor>>,
    settings: AudioSettings,
    preview: bool,
    retry_at: Option<Instant>,
    recoveries: u64,
}
impl RecoveringProcessor {
    fn new(settings: &AudioSettings, preview: bool) -> Self {
        Self {
            inner: None,
            settings: settings.clone(),
            preview,
            retry_at: None,
            recoveries: 0,
        }
    }
    fn process<T>(
        &mut self,
        operation: impl FnOnce(&mut Processor) -> Result<T, String>,
    ) -> Result<T, String> {
        if let Some(at) = self.retry_at {
            if Instant::now() < at {
                return Err("声音处理正在自动恢复…".into());
            }
            self.inner = None;
            self.recoveries += 1;
            self.retry_at = None;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let processor = self
                .inner
                .get_or_insert_with(|| Box::new(Processor::new(&self.settings, self.preview)));
            operation(processor)
        }))
        .unwrap_or_else(|_| Err("声音处理发生异常".into()));
        if result.is_err() {
            self.retry_at = Some(Instant::now() + Duration::from_millis(100));
        }
        result
    }
    fn render(&mut self, frame: &Frame) -> Result<(), String> {
        self.process(|processor| processor.render(frame))
    }
    fn capture(
        &mut self,
        frame: &mut Frame,
        echo_delay: Option<u16>,
    ) -> Result<Option<f32>, String> {
        self.process(|processor| {
            if let Some(delay) = echo_delay {
                processor.set_echo_delay(delay);
            }
            processor.capture(frame)
        })
    }
    fn applied_gain(&self) -> Option<f32> {
        self.inner.as_ref().map(|processor| processor.input_gain)
    }
    fn reset(&mut self, settings: &AudioSettings, preview: bool) {
        let recoveries = self.recoveries;
        *self = Self::new(settings, preview);
        self.recoveries = recoveries;
    }
}

fn trim_capture_backlog(samples: &mut VecDeque<f32>) -> bool {
    if samples.len() <= 4800 {
        return false;
    }
    // Retain the newest 40 ms instead of dropping every frame in a large callback.
    // This prevents a slow callback cadence from permanently starving voice activation.
    samples.drain(..samples.len() - 1920);
    true
}

#[derive(Default)]
pub struct VoiceGate {
    hangover: usize,
    consecutive_speech: u8,
}
impl VoiceGate {
    pub fn open(&mut self, settings: &AudioSettings, ptt: bool, probability: Option<f32>) -> bool {
        if settings.input_muted || settings.output_muted {
            *self = Self::default();
            return false;
        }
        match settings.talk_mode {
            TalkMode::Continuous => true,
            TalkMode::PushToTalk => ptt,
            TalkMode::VoiceActivation => {
                let threshold = 0.98 - settings.voice_activation_sensitivity * 0.004;
                let score = probability.unwrap_or(0.);
                if score >= threshold {
                    self.consecutive_speech = self.consecutive_speech.saturating_add(1);
                } else {
                    self.consecutive_speech = 0;
                }
                // Three 10 ms frames confirm speech. Once open, lower the threshold until the
                // 250 ms tail expires, so a quiet syllable or a short pause stays audible.
                if self.consecutive_speech >= 3 || (self.hangover > 0 && score >= threshold - 0.12)
                {
                    self.hangover = 25;
                } else {
                    self.hangover = self.hangover.saturating_sub(1);
                }
                self.hangover > 0
            }
        }
    }
}
fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}
fn encoded_sender(music: bool) -> Result<Encoder, String> {
    let mut encoder = Encoder::new(
        SampleRate::Hz48000,
        Channels::Mono,
        if music {
            Application::Audio
        } else {
            Application::Voip
        },
    )
    .map_err(|e| e.to_string())?;
    encoder
        .set_bitrate(Bitrate::BitsPerSecond(32000))
        .map_err(|e| e.to_string())?;
    if !music {
        encoder
            .set_signal(Signal::Voice)
            .map_err(|e| e.to_string())?;
        encoder.set_inband_fec(true).map_err(|e| e.to_string())?;
        encoder.set_packet_loss_perc(8).map_err(|e| e.to_string())?;
    }
    Ok(encoder)
}

fn worker(
    rx: mpsc::Receiver<Work>,
    controls: mpsc::Receiver<Control>,
    outgoing: tokio::sync::mpsc::Sender<EncodedAudio>,
    mut settings: AudioSettings,
    shared: Arc<Mutex<AudioStatus>>,
    muted: Arc<AtomicBool>,
) {
    let mut state = AudioStatus::default();
    let mut running = false;
    let mut preview = false;
    let mut epoch = 0;
    let mut ptt = false;
    let mut can_send = false;
    let mut can_receive = false;
    let mut music = None;
    let mut input: Option<Input> = None;
    let mut output: Option<Output> = None;
    let mut input_resample = Resampler::default();
    let mut reference_resample = Resampler::default();
    let mut left_resample = Resampler::default();
    let mut right_resample = Resampler::default();
    let mut input_pcm = VecDeque::new();
    let mut reference_pcm = VecDeque::new();
    let mut monitor = VecDeque::new();
    let mut output_left = Vec::new();
    let mut output_right = Vec::new();
    let mut handler = Playback::new();
    let mut ignored = HashSet::new();
    let mut volumes = std::collections::HashMap::<u16, f32>::new();
    let mut processor = RecoveringProcessor::new(&settings, false);
    let mut encoder: Option<Encoder> = None;
    let mut bitrate = 32_000;
    let mut gate = VoiceGate::default();
    let mut speech = Vec::with_capacity(FRAME_SAMPLES * 3);
    let mut previous = [[0.; FRAME_SAMPLES]; 2];
    let mut sending = false;
    let mut test_remaining = 0usize;
    let mut presence_tone = None;
    let mut test_phase = 0f32;
    let mut retry = Instant::now();
    let mut last_status = Instant::now();
    loop {
        let mut reset = false;
        for _ in 0..128 {
            let next = match controls.try_recv() {
                Ok(command) => Ok(Work::Control(command)),
                Err(mpsc::TryRecvError::Empty) => rx.try_recv(),
                Err(mpsc::TryRecvError::Disconnected) => return,
            };
            match next {
                Ok(Work::Packet(packet)) => {
                    state.received_packets += 1;
                    if !running || !can_receive || settings.output_muted {
                        continue;
                    }
                    let from = match packet.data().data() {
                        AudioData::S2C { from, .. } | AudioData::S2CWhisper { from, .. } => *from,
                        _ => continue,
                    };
                    if !ignored.contains(&from) {
                        handler.receive(from, packet, volumes.get(&from).copied().unwrap_or(1.));
                    }
                }
                Ok(Work::Control(command)) => match command {
                    Control::RemovePeer(id) => {
                        volumes.remove(&id);
                        handler.remove_client(id);
                    }
                    Control::ClearPeers => {
                        volumes.clear();
                        ignored.clear();
                        handler.reset();
                        while rx.try_recv().is_ok() {}
                    }
                    Control::IgnoreClients(clients) => {
                        ignored = clients;
                        handler.remove_ignored(&ignored);
                    }
                    Control::UserVolume(id, volume) => {
                        volumes.insert(id, volume);
                        handler.set_volume(id, volume);
                    }
                    Control::Start => {
                        running = true;
                        state.received_packets = 0;
                        reset = true;
                    }
                    Control::Stop => {
                        presence_tone = None;
                        volumes.clear();
                        ignored.clear();
                        running = false;
                        preview = false;
                        ptt = false;
                        can_send = false;
                        can_receive = false;
                        music = None;
                        reset = true;
                    }
                    Control::Configure(s, e) => {
                        if settings.changes_stream_processing(&s) {
                            reset = true;
                        }
                        if epoch != e {
                            ptt = false;
                            speech.clear();
                            sending = false;
                            previous.fill([0.; FRAME_SAMPLES]);
                            gate = VoiceGate::default();
                        }
                        settings = s;
                        epoch = e;
                    }
                    Control::Ptt(held, e) => {
                        if let Some(capture) = &mut input {
                            while capture.samples.try_pop().is_some() {}
                        }
                        ptt = held;
                        epoch = e;
                        speech.clear();
                        sending = false;
                        gate = VoiceGate::default();
                        input_pcm.clear();
                        previous.fill([0.; FRAME_SAMPLES]);
                    }
                    Control::MicTest(enabled, e) => {
                        ptt = false;
                        preview = enabled;
                        epoch = e;
                        reset = true;
                    }
                    Control::PresenceTone(joined) => {
                        if running && !settings.output_muted {
                            presence_tone = Some((joined, 0usize));
                        }
                    }
                    Control::TestSpeaker => {
                        test_remaining = 24000;
                        retry = Instant::now() - Duration::from_secs(2);
                    }
                    Control::Restart(e) => {
                        epoch = e;
                        reset = true;
                        state.recovery_notice =
                            Some("已手动重启音频，连接和发言设置保持不变".into());
                    }
                    Control::Permissions {
                        send,
                        receive,
                        music: m,
                        quality,
                        epoch: e,
                    } => {
                        if can_send != send || music != m || epoch != e {
                            gate = VoiceGate::default();
                            input_pcm.clear();
                            if let Some(capture) = &mut input {
                                while capture.samples.try_pop().is_some() {}
                            }
                            if epoch != e {
                                handler.reset();
                            }
                            speech.clear();
                            sending = false;
                            previous.fill([0.; FRAME_SAMPLES]);
                        }
                        if music != m {
                            encoder = None;
                        }
                        can_send = send;
                        can_receive = receive;
                        music = m;
                        epoch = e;
                        let rate = if m == Some(true) {
                            48000 + u32::from(quality.min(10)) * 8000
                        } else {
                            12000 + u32::from(quality.min(10)) * 4000
                        };
                        if bitrate != rate as i32 {
                            bitrate = rate as i32;
                            if let Some(encoder) = &mut encoder {
                                let _ = encoder.set_bitrate(Bitrate::BitsPerSecond(bitrate));
                            }
                        }
                        if !receive {
                            handler.reset();
                        }
                    }
                },
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }
        if reset {
            encoder = None;
            input = None;
            output = None;
            handler.reset();
            input_pcm.clear();
            reference_pcm.clear();
            monitor.clear();
            speech.clear();
            previous.fill([0.; FRAME_SAMPLES]);
            sending = false;
            input_resample = Resampler::default();
            reference_resample = Resampler::default();
            left_resample = Resampler::default();
            right_resample = Resampler::default();
            processor.reset(&settings, preview);
            gate = VoiceGate::default();
            retry = Instant::now();
            state.input_error = None;
            state.output_error = None;
            state.processing_error = None;
        }
        let need_output = running || preview || test_remaining > 0;
        let need_input = preview || (running && !settings.input_muted && !settings.output_muted);
        if !need_input {
            input = None;
            input_pcm.clear();
            state.input_level = 0.;
            state.raw_input_level = 0.;
            state.voice_probability = None;
            state.applied_input_gain = None;
        }
        if !need_output {
            output = None;
            state.output_level = 0.;
        }
        let now = Instant::now();
        let mut reset_processing = false;
        if input.as_mut().is_some_and(|s| {
            s.health.failed.load(Ordering::Relaxed)
                || s.watchdog
                    .stalled(s.health.callbacks.load(Ordering::Relaxed), now)
        }) {
            input = None;
            input_pcm.clear();
            reset_processing = true;
            state.device_recoveries += 1;
            state.input_error = Some("麦克风回调中断，正在自动重建采集流".into());
        }
        if output.as_mut().is_some_and(|s| {
            s.health.failed.load(Ordering::Relaxed)
                || s.watchdog
                    .stalled(s.health.callbacks.load(Ordering::Relaxed), now)
        }) {
            output = None;
            reference_pcm.clear();
            reset_processing = true;
            state.device_recoveries += 1;
            state.output_error = Some("播放回调中断，正在自动重建播放流".into());
        }
        if retry.elapsed() >= Duration::from_secs(2) || reset {
            retry = Instant::now();
            if settings.input_device.is_none()
                && let Some(current) = &input
                && device(&None, true)
                    .and_then(|d| d.id().map(|id| id.to_string()).map_err(|e| e.to_string()))
                    .ok()
                    .as_deref()
                    != Some(current.id.as_str())
            {
                input = None;
                input_pcm.clear();
                input_resample = Resampler::default();
                reset_processing = true;
            }
            if settings.output_device.is_none()
                && let Some(current) = &output
                && device(&None, false)
                    .and_then(|d| d.id().map(|id| id.to_string()).map_err(|e| e.to_string()))
                    .ok()
                    .as_deref()
                    != Some(current.id.as_str())
            {
                output = None;
                reference_pcm.clear();
                reset_processing = true;
            }
            if need_output && output.is_none() {
                match open_output(&settings, muted.clone()) {
                    Ok(s) => {
                        output = Some(s);
                        reset_processing = true;
                        state.output_error = None;
                        left_resample = Resampler::default();
                        right_resample = Resampler::default();
                        reference_resample = Resampler::default();
                        reference_pcm.clear();
                    }
                    Err(e) => state.output_error = Some(e),
                }
            }
            if need_input && input.is_none() {
                match open_input(&settings) {
                    Ok(s) => {
                        input = Some(s);
                        reset_processing = true;
                        state.input_error = None;
                        input_resample = Resampler::default();
                        input_pcm.clear();
                    }
                    Err(e) => state.input_error = Some(e),
                }
            }
        }
        if reset_processing {
            processor.reset(&settings, preview);
            input_resample = Resampler::default();
            reference_resample = Resampler::default();
            input_pcm.clear();
            reference_pcm.clear();
            speech.clear();
            previous.fill([0.; FRAME_SAMPLES]);
            sending = false;
            gate = VoiceGate::default();
        }
        if let Some(out) = &mut output {
            if input.is_some() && !preview && settings.echo_cancellation {
                while let Some(sample) = out.reference.try_pop() {
                    reference_resample
                        .push(sample, out.rate, 48000, |s| reference_pcm.push_back(s));
                }
                while reference_pcm.len() >= FRAME_SAMPLES {
                    let mut frame = [0.; FRAME_SAMPLES];
                    for s in &mut frame {
                        *s = reference_pcm.pop_front().unwrap();
                    }
                    if let Err(e) = processor.render(&frame) {
                        state.processing_error = Some(e);
                    }
                }
            } else {
                out.reference.skip(out.reference.occupied_len());
            }
        }
        state.echo_delay_ms = (settings.echo_cancellation && !preview)
            .then(|| input.as_ref().zip(output.as_ref()))
            .flatten()
            .map(|(capture, playback)| {
                (capture.health.delay_ms.load(Ordering::Relaxed)
                    + playback.health.delay_ms.load(Ordering::Relaxed))
                .min(500) as u16
            });
        if let Some(capture) = &mut input {
            state.input_clipped_samples +=
                capture.health.clipped_samples.swap(0, Ordering::Relaxed);
            let lost = capture.health.dropped_samples.swap(0, Ordering::Relaxed);
            if lost > 0 {
                let stale = capture.samples.occupied_len();
                capture.samples.skip(stale);
                state.capture_dropped_samples += lost + stale as u64 + input_pcm.len() as u64;
                input_resample = Resampler::default();
                input_pcm.clear();
            }
            while let Some(sample) = capture.samples.try_pop() {
                input_resample.push(sample, capture.rate, 48_000, |s| input_pcm.push_back(s));
            }
            let queued = input_pcm.len();
            let trimmed = trim_capture_backlog(&mut input_pcm);
            if trimmed || lost > 0 {
                state.capture_dropped_samples += (queued - input_pcm.len()) as u64;
                // A sample gap must end the current Opus burst so the decoder can start fresh.
                // Do not carry buffered syllables across a gap in the device stream.
                if sending {
                    let _ = outgoing.try_send(EncodedAudio {
                        epoch,
                        created: Instant::now(),
                        bytes: Vec::new(),
                        music: music == Some(true),
                    });
                }
                sending = false;
                speech.clear();
                previous.fill([0.; FRAME_SAMPLES]);
                encoder = None;
                gate = VoiceGate::default();
                state.recovery_notice = Some("采集发生积压，已从新的音频帧恢复".into());
            }
            while input_pcm.len() >= FRAME_SAMPLES {
                let mut frame = [0.; FRAME_SAMPLES];
                for s in &mut frame {
                    *s = input_pcm.pop_front().unwrap();
                }
                state.raw_input_level = rms(&frame);
                let probability = match processor.capture(&mut frame, state.echo_delay_ms) {
                    Ok(probability) => probability,
                    Err(e) => {
                        state.processing_error = Some(e);
                        state.input_level = 0.;
                        state.voice_probability = None;
                        if sending {
                            let _ = outgoing.try_send(EncodedAudio {
                                epoch,
                                created: Instant::now(),
                                bytes: vec![],
                                music: music == Some(true),
                            });
                        }
                        sending = false;
                        speech.clear();
                        previous.fill([0.; FRAME_SAMPLES]);
                        gate = VoiceGate::default();
                        continue;
                    }
                };
                state.voice_probability = probability;
                state.applied_input_gain = processor.applied_gain();
                state.processing_error = None;
                state.dsp_recoveries = processor.recoveries;
                state.input_level = rms(&frame);
                if preview && monitor.len() < 4800 {
                    monitor.extend(frame);
                }
                let open = !preview
                    && can_send
                    && music.is_some()
                    && gate.open(&settings, ptt, probability);
                if open {
                    if !sending {
                        if settings.talk_mode == TalkMode::VoiceActivation {
                            speech.extend_from_slice(&previous[0]);
                        }
                        speech.extend_from_slice(&previous[1]);
                        sending = true;
                    }
                    speech.extend_from_slice(&frame);
                    if speech.len() >= 960 {
                        let mut encoded = [0u8; 1275];
                        let result = (|| {
                            if encoder.is_none() {
                                let mut new_encoder = encoded_sender(music == Some(true))?;
                                new_encoder
                                    .set_bitrate(Bitrate::BitsPerSecond(bitrate))
                                    .map_err(|e| e.to_string())?;
                                encoder = Some(new_encoder);
                            }
                            encoder
                                .as_ref()
                                .expect("encoder initialized")
                                .encode_float(&speech[..960], &mut encoded)
                                .map_err(|e| e.to_string())
                        })();
                        match result {
                            Ok(length) => {
                                state.encoded_packets += 1;
                                if outgoing
                                    .try_send(EncodedAudio {
                                        epoch,
                                        created: Instant::now(),
                                        bytes: encoded[..length].to_vec(),
                                        music: music == Some(true),
                                    })
                                    .is_err()
                                {
                                    state.outgoing_drops += 1;
                                }
                            }
                            Err(e) => {
                                state.processing_error =
                                    Some(format!("语音编码异常，正在恢复：{e}"));
                                encoder = None;
                                speech.clear();
                                sending = false;
                            }
                        }
                        if speech.len() >= 960 {
                            speech.drain(..960);
                        }
                    }
                } else if sending {
                    let _ = outgoing.try_send(EncodedAudio {
                        epoch,
                        created: Instant::now(),
                        bytes: vec![],
                        music: music == Some(true),
                    });
                    sending = false;
                    speech.clear();
                }
                previous = [previous[1], frame];
            }
        }
        if let Some(out) = &mut output {
            let target = out.rate as usize * 2 * 30 / 1000;
            while out.samples.occupied_len() < target {
                let mut mix = [0f32; 960];
                if can_receive && !settings.output_muted {
                    handler.fill_buffer(&mut mix);
                }
                if preview {
                    for pair in mix.as_chunks_mut::<2>().0 {
                        let s = monitor.pop_front().unwrap_or(0.);
                        pair[0] += s;
                        pair[1] += s;
                    }
                }
                if let Some((joined, offset)) = presence_tone {
                    const NOTE: usize = 3840;
                    for (i, pair) in mix.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                        let pos = offset + i;
                        if pos >= NOTE * 2 {
                            break;
                        }
                        let freq = if (pos < NOTE) == joined {
                            523.25
                        } else {
                            698.46
                        };
                        let t = (pos % NOTE) as f32;
                        let envelope = (t / 240.).min(1.) * (1. - t / NOTE as f32);
                        let sample =
                            0.09 * envelope * (std::f32::consts::TAU * freq * t / 48000.).sin();
                        pair[0] += sample;
                        pair[1] += sample;
                    }
                    let next = offset + mix.len() / 2;
                    presence_tone = (next < NOTE * 2).then_some((joined, next));
                }
                if test_remaining > 0 {
                    for pair in mix.as_chunks_mut::<2>().0 {
                        if test_remaining == 0 {
                            break;
                        }
                        let s = (test_phase * std::f32::consts::TAU).sin() * 0.08;
                        test_phase = (test_phase + 440. / 48000.) % 1.;
                        pair[0] += s;
                        pair[1] += s;
                        test_remaining -= 1;
                    }
                }
                for s in &mut mix {
                    *s = if settings.output_muted {
                        0.
                    } else {
                        (*s * settings.output_volume).clamp(-1., 1.)
                    };
                }
                state.output_level = rms(&mix);
                output_left.clear();
                output_right.clear();
                for pair in mix.as_chunks::<2>().0 {
                    left_resample.push(pair[0], 48000, out.rate, |s| output_left.push(s));
                    right_resample.push(pair[1], 48000, out.rate, |s| output_right.push(s));
                }
                for (l, r) in output_left
                    .iter()
                    .copied()
                    .zip(output_right.iter().copied())
                {
                    let _ = out.samples.try_push(l);
                    let _ = out.samples.try_push(r);
                }
            }
        }
        state.can_receive = can_receive;
        state.can_send = can_send;
        state.running = running;
        state.input_ready = input.is_some();
        state.output_ready = output.is_some();
        state.mic_test = preview;
        state.transmitting = sending;
        state.speakers = handler.speakers();
        state.discarded_packets = handler.discarded;
        state.playback_recoveries = handler.recoveries;
        if last_status.elapsed() > Duration::from_millis(80) {
            *shared.lock().unwrap_or_else(|e| e.into_inner()) = state.clone();
            last_status = Instant::now();
        }
        thread::sleep(Duration::from_millis(
            if running || preview || test_remaining > 0 {
                2
            } else {
                20
            },
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_watchdog_distinguishes_silent_callbacks_from_stopped_callbacks() {
        let start = Instant::now();
        let mut watch = CallbackWatch::new(start);
        for second in 1..20 {
            assert!(!watch.stalled(second, start + Duration::from_secs(second)));
        }
        assert!(watch.stalled(19, start + Duration::from_secs(22)));
        assert!(!watch.stalled(20, start + Duration::from_secs(23)));
    }
    #[test]
    fn capture_backlog_retains_recent_voice_instead_of_starving_activation() {
        let mut samples: VecDeque<_> = (0..9600).map(|s| s as f32).collect();
        assert!(trim_capture_backlog(&mut samples));
        assert_eq!(samples.len(), 1920);
        assert_eq!(samples.front().copied(), Some(7680.));
        assert_eq!(samples.back().copied(), Some(9599.));
    }
    #[test]
    fn dsp_fault_recovers_without_changing_voice_settings() {
        thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let settings = AudioSettings {
                    input_muted: false,
                    noise_mode: NoiseMode::Off,
                    echo_cancellation: false,
                    auto_gain: false,
                    talk_mode: TalkMode::VoiceActivation,
                    ..Default::default()
                };
                let mut dsp = RecoveringProcessor::new(&settings, false);
                assert!(dsp.capture(&mut [f32::NAN; FRAME_SAMPLES], None).is_err());
                thread::sleep(Duration::from_millis(120));
                let mut gate = VoiceGate::default();
                let mut open = 0;
                for i in 0..100 {
                    let mut voice = std::array::from_fn(|n| {
                        ((i * FRAME_SAMPLES + n) as f32 * 0.0576).sin() * 0.05
                    });
                    dsp.render(&[0.; FRAME_SAMPLES]).unwrap();
                    assert!(dsp.capture(&mut voice, None).unwrap().is_some());
                    if gate.open(&settings, false, Some(0.95)) {
                        open += 1;
                    }
                }
                assert_eq!(dsp.recoveries, 1);
                assert!(open > 90, "activation must resume after rebuilding DSP");
            })
            .unwrap()
            .join()
            .unwrap();
    }
    #[test]
    fn voice_activation_survives_loss_and_bursts_on_the_receive_path() {
        thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                use tsproto_packets::packets::{CodecType, Direction, OutAudio};
                let settings = AudioSettings {
                    input_muted: false,
                    talk_mode: TalkMode::VoiceActivation,
                    ..Default::default()
                };
                let mut dsp = RecoveringProcessor::new(&settings, false);
                let mut playback = Playback::new();
                let mut gate = VoiceGate::default();
                let encoder = encoded_sender(false).unwrap();
                let source: Vec<_> = (0..960).map(|n| (n as f32 * 0.0576).sin() * 0.02).collect();
                let mut bytes = [0; 1275];
                let len = encoder.encode_float(&source, &mut bytes).unwrap();
                let mut open_at_end = 0;
                for i in 0..1000usize {
                    if i % 2 == 0 && i % 26 != 0 {
                        let id = (i / 2) as u16;
                        let packet = OutAudio::new(&AudioData::S2C {
                            id,
                            codec: CodecType::OpusVoice,
                            from: 9,
                            data: &bytes[..len],
                        });
                        playback.receive(
                            9,
                            InAudioBuf::try_new(Direction::S2C, packet.into_vec()).unwrap(),
                            1.,
                        );
                    }
                    let mut mix = [0.; 960];
                    playback.fill_buffer(&mut mix);
                    let reference = std::array::from_fn(|n| (mix[n * 2] + mix[n * 2 + 1]) * 0.5);
                    dsp.render(&reference).unwrap();
                    // Different capture/render signals simulate near-end speech and another talker.
                    let mut capture = std::array::from_fn(|n| {
                        ((i * FRAME_SAMPLES + n) as f32 * 0.035).sin() * 0.12
                    });
                    assert!(dsp.capture(&mut capture, None).unwrap().is_some());
                    if gate.open(&settings, false, Some(0.95)) && i > 900 {
                        open_at_end += 1;
                    }
                }
                assert!(
                    open_at_end > 80,
                    "activation stopped after receive losses: {open_at_end}"
                );
            })
            .unwrap()
            .join()
            .unwrap();
    }
    #[test]
    fn transient_audio_notifications_keep_stream_alive() {
        use cpal::ErrorKind::*;
        for kind in [Xrun, DeviceChanged, RealtimeDenied] {
            assert!(!stream_needs_restart(kind));
        }
        for kind in [
            DeviceNotAvailable,
            StreamInvalidated,
            PermissionDenied,
            BackendError,
        ] {
            assert!(stream_needs_restart(kind));
        }
    }
    #[test]
    fn resampling_is_continuous_across_callback_boundaries() {
        let mut r = Resampler::default();
        let mut out = vec![];
        for chunk in (0..44100).collect::<Vec<_>>().chunks(137) {
            for n in chunk {
                r.push(*n as f32, 44100, 48000, |s| out.push(s));
            }
        }
        assert!((out.len() as i32 - 48000).abs() <= 2);
        assert!(out.windows(2).all(|w| w[1] >= w[0]));
    }
    #[test]
    fn gate_never_sends_when_muted_and_hangover_expires() {
        let mut s = AudioSettings {
            input_muted: false,
            talk_mode: TalkMode::VoiceActivation,
            ..Default::default()
        };
        let mut g = VoiceGate::default();
        assert!(!g.open(&s, false, Some(0.95)));
        assert!(!g.open(&s, false, Some(0.01)));
        assert!(!g.open(&s, false, Some(0.95)));
        assert!(!g.open(&s, false, Some(0.95)));
        assert!(g.open(&s, false, Some(0.95)));
        for _ in 0..24 {
            assert!(g.open(&s, false, Some(0.)));
        }
        assert!(!g.open(&s, false, Some(0.)));
        s.input_muted = true;
        assert!(!g.open(&s, true, Some(1.)));
        s.input_muted = false;
        s.output_muted = true;
        assert!(!g.open(&s, true, Some(1.)));
    }
    #[test]
    fn loud_broadband_noise_does_not_open_the_speech_gate() {
        thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let settings = AudioSettings {
                    talk_mode: TalkMode::VoiceActivation,
                    input_muted: false,
                    noise_mode: NoiseMode::Off,
                    echo_cancellation: false,
                    auto_gain: false,
                    ..Default::default()
                };
                let mut processor = Processor::new(&settings, false);
                let mut gate = VoiceGate::default();
                let mut seed = 8u32;
                for _ in 0..150 {
                    let mut noise = std::array::from_fn(|_| {
                        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                        (seed as f32 / u32::MAX as f32 - 0.5) * 0.35
                    });
                    let probability = processor.capture(&mut noise).unwrap();
                    assert!(rms(&noise) > 0.05);
                    assert!(
                        !gate.open(&settings, false, probability),
                        "noise probability {probability:?}"
                    );
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    #[ignore = "Requires a local 48 kHz, mono, signed 16-bit little-endian speech fixture"]
    fn speech_fixture_opens_at_default_sensitivity() {
        let path = std::env::var_os("CONGSPEAK_SPEECH_PCM").expect("speech fixture path");
        let pcm = std::fs::read(path).unwrap();
        assert!(pcm.len() >= FRAME_SAMPLES * 2 * 100);
        thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                for mode in [NoiseMode::Off, NoiseMode::Standard, NoiseMode::Rnnoise] {
                    let settings = AudioSettings {
                        talk_mode: TalkMode::VoiceActivation,
                        input_muted: false,
                        noise_mode: mode.clone(),
                        ..Default::default()
                    };
                    let mut processor = Processor::new(&settings, false);
                    let mut gate = VoiceGate::default();
                    let mut opened = 0;
                    for bytes in pcm.as_chunks::<{ FRAME_SAMPLES * 2 }>().0 {
                        let mut frame: Frame = std::array::from_fn(|i| {
                            i16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]) as f32 / 32768.
                                * 0.35
                        });
                        let probability = processor.capture(&mut frame).unwrap();
                        opened += usize::from(gate.open(&settings, false, probability));
                    }
                    assert!(opened > 50, "{mode:?}: opened {opened} speech frames");
                    for _ in 0..100 {
                        let probability = processor.capture(&mut [0.; FRAME_SAMPLES]).unwrap();
                        gate.open(&settings, false, probability);
                    }
                    assert!(!gate.open(&settings, false, Some(0.)));
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn high_input_gain_preserves_waveform_and_recovers_gradually() {
        thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let settings = AudioSettings {
                    input_gain: 3.,
                    noise_mode: NoiseMode::Off,
                    auto_gain: false,
                    echo_cancellation: false,
                    ..Default::default()
                };
                let mut processor = Processor::new(&settings, false);
                let tone = |i: usize| (i as f32 * std::f32::consts::TAU / 48.).sin();
                let mut output = [0.; FRAME_SAMPLES];
                for _ in 0..12 {
                    output = std::array::from_fn(|i| tone(i) * 0.7);
                    processor.capture(&mut output).unwrap();
                }
                let fundamental = output
                    .iter()
                    .enumerate()
                    .map(|(i, s)| s * tone(i))
                    .sum::<f32>();
                let third = output
                    .iter()
                    .enumerate()
                    .map(|(i, s)| s * tone(i * 3))
                    .sum::<f32>();
                assert!(fundamental.abs() > 150.);
                assert!(
                    third.abs() / fundamental.abs() < 0.02,
                    "unexpected clipping harmonic"
                );
                assert!(processor.input_gain < settings.input_gain);
                let limited = processor.input_gain;
                processor.capture(&mut [0.01; FRAME_SAMPLES]).unwrap();
                assert!(processor.input_gain > limited && processor.input_gain < limited + 0.1);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn opus_loss_resilience_is_enabled_only_for_voice_channels() {
        let voice = encoded_sender(false).unwrap();
        assert!(voice.inband_fec().unwrap());
        assert_eq!(voice.packet_loss_perc().unwrap(), 8);
        let music = encoded_sender(true).unwrap();
        assert!(!music.inband_fec().unwrap());
        assert_eq!(music.packet_loss_perc().unwrap(), 0);
    }

    #[test]
    fn dsp_and_opus_roundtrip() {
        thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let settings = AudioSettings::default();
                let mut p = Processor::new(&settings, false);
                let encoder = encoded_sender(false).unwrap();
                let mut decoder =
                    audiopus::coder::Decoder::new(SampleRate::Hz48000, Channels::Mono).unwrap();
                for _ in 0..30 {
                    let mut f = [0.01; 480];
                    p.render(&[0.; 480]).unwrap();
                    p.capture(&mut f).unwrap();
                    assert!(f.iter().all(|s| s.is_finite()));
                }
                let source: Vec<_> = (0..960).map(|i| (i as f32 * 0.0576).sin() * 0.1).collect();
                let mut encoded = [0; 1275];
                let n = encoder.encode_float(&source, &mut encoded).unwrap();
                let mut pcm = [0.; 960];
                let samples = decoder
                    .decode_float(
                        Some((&encoded[..n]).try_into().unwrap()),
                        (&mut pcm[..]).try_into().unwrap(),
                        false,
                    )
                    .unwrap();
                assert_eq!(samples, 960);
                assert!(rms(&pcm) > 0.001);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
