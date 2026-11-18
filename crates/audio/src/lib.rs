pub const SAMPLE_RATE: u32 = 48_000;
pub const FRAME_SAMPLES: usize = 480;
pub type Frame = [f32; FRAME_SAMPLES];

#[cfg(feature = "runtime")]
mod playback;
#[cfg(feature = "runtime")]
pub mod runtime;

/// All public audio samples use normalized [-1, 1] f32 PCM.
pub struct RnNoise {
    state: Box<nnnoiseless::DenoiseState<'static>>,
    input: Frame,
    output: Frame,
}

impl Default for RnNoise {
    fn default() -> Self {
        Self {
            state: nnnoiseless::DenoiseState::new(),
            input: [0.; FRAME_SAMPLES],
            output: [0.; FRAME_SAMPLES],
        }
    }
}

impl RnNoise {
    /// RNNoise expects i16-scale floating point samples, unlike CPAL and WebRTC.
    pub fn process(&mut self, frame: &mut Frame) -> f32 {
        for (destination, sample) in self.input.iter_mut().zip(frame.iter()) {
            *destination = if sample.is_finite() {
                sample.clamp(-1., 1.) * 32768.
            } else {
                0.
            };
        }
        let probability = self.state.process_frame(&mut self.output, &self.input);
        for (destination, sample) in frame.iter_mut().zip(self.output.iter()) {
            *destination = (sample / 32768.).clamp(-1., 1.);
        }
        probability
    }
}

/// Fixed-size accumulation: input callback sizes need not match DSP frame boundaries.
#[derive(Default)]
pub struct FrameAccumulator {
    samples: Vec<f32>,
}

impl FrameAccumulator {
    pub fn new() -> Self {
        Self {
            samples: Vec::with_capacity(FRAME_SAMPLES),
        }
    }

    /// Call on the audio worker, not on a device callback. At most one partial frame is retained.
    pub fn push(&mut self, samples: &[f32], mut consume: impl FnMut(&Frame)) {
        for sample in samples {
            self.samples.push(*sample);
            if self.samples.len() == FRAME_SAMPLES {
                let frame: &Frame = self
                    .samples
                    .as_slice()
                    .try_into()
                    .expect("fixed frame size");
                consume(frame);
                self.samples.clear();
            }
        }
    }

    pub fn reset(&mut self) {
        self.samples.clear();
    }
}

#[cfg(feature = "webrtc")]
pub struct WebRtc {
    processor: webrtc_audio_processing::Processor,
}

#[cfg(feature = "webrtc")]
impl WebRtc {
    pub fn new(
        noise_suppression: bool,
        echo_cancellation: bool,
        auto_gain: bool,
    ) -> Result<Self, String> {
        use webrtc_audio_processing::config::*;
        let processor = webrtc_audio_processing::Processor::new(SAMPLE_RATE)
            .map_err(|error| error.to_string())?;
        processor.set_config(Config {
            noise_suppression: noise_suppression.then(NoiseSuppression::default),
            echo_canceller: echo_cancellation.then(EchoCanceller::default),
            gain_controller: auto_gain.then(|| {
                GainController::GainController2(GainController2 {
                    adaptive_digital: Some(AdaptiveDigital {
                        max_gain_db: 20.,
                        initial_gain_db: 0.,
                        ..Default::default()
                    }),
                    ..Default::default()
                })
            }),
            high_pass_filter: Some(HighPassFilter::default()),
            ..Default::default()
        });
        Ok(Self { processor })
    }

    pub fn render(&self, frame: &Frame) -> Result<(), String> {
        self.processor
            .analyze_render_frame([frame.as_slice()])
            .map_err(|error| error.to_string())
    }

    pub fn capture(&self, frame: &mut Frame) -> Result<(), String> {
        self.processor
            .process_capture_frame([frame.as_mut_slice()])
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irregular_callbacks_preserve_samples_and_reset_discards_stale_audio() {
        let mut accumulator = FrameAccumulator::new();
        let input: Vec<_> = (0..1200).map(|sample| sample as f32).collect();
        let mut frames = Vec::new();
        for chunk in input.chunks(137) {
            accumulator.push(chunk, |frame| frames.push(*frame));
        }
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].as_slice(), &input[..480]);
        assert_eq!(frames[1].as_slice(), &input[480..960]);
        accumulator.reset();
        accumulator.push(&[9.; FRAME_SAMPLES], |frame| {
            assert!(frame.iter().all(|sample| *sample == 9.))
        });
    }

    #[test]
    fn rnnoise_silence_stays_finite_and_normalized() {
        let mut denoiser = RnNoise::default();
        for _ in 0..20 {
            let mut frame = [0.; FRAME_SAMPLES];
            let probability = denoiser.process(&mut frame);
            assert!(probability.is_finite());
            assert!(
                frame
                    .iter()
                    .all(|sample| sample.is_finite() && sample.abs() < 0.001)
            );
        }
    }
}
