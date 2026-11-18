//! The network decoder is a fault boundary: a damaged receive queue must never
//! unwind through the capture/encode loop running on the same audio worker.
use std::panic::{AssertUnwindSafe, catch_unwind};
use tsclientlib::audio::AudioHandler;
use tsproto_packets::packets::InAudioBuf;

#[derive(Default)]
pub(crate) struct Playback {
    inner: AudioHandler<u16>,
    pub discarded: u64,
    pub recoveries: u64,
}
impl Playback {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn reset(&mut self) {
        self.inner = AudioHandler::new();
    }
    pub fn receive(&mut self, id: u16, packet: InAudioBuf, volume: f32) {
        if self.inner.get_queues().len() >= 64 && !self.inner.get_queues().contains_key(&id) {
            self.discarded += 1;
            return;
        }
        match catch_unwind(AssertUnwindSafe(|| self.inner.handle_packet(id, packet))) {
            Ok(Ok(_)) => self.set_volume(id, volume),
            // Late/duplicate/invalid packets are receive-side drops, not microphone faults.
            Ok(Err(_)) => self.discarded += 1,
            Err(_) => self.recover(),
        }
    }
    pub fn remove_client(&mut self, id: u16) {
        self.inner.get_mut_queues().remove(&id);
    }
    pub fn remove_ignored(&mut self, clients: &std::collections::HashSet<u16>) {
        self.inner
            .get_mut_queues()
            .retain(|id, _| !clients.contains(id));
    }
    pub fn set_volume(&mut self, id: u16, volume: f32) {
        if let Some(queue) = self.inner.get_mut_queues().get_mut(&id) {
            queue.volume = volume;
        }
    }
    pub fn fill_buffer(&mut self, output: &mut [f32]) {
        let result = catch_unwind(AssertUnwindSafe(|| self.inner.fill_buffer(output)));
        if result.is_err() || output.iter().any(|s| !s.is_finite()) {
            output.fill(0.);
            self.recover();
        }
    }
    pub fn speakers(&self) -> Vec<u16> {
        self.inner.get_queues().keys().copied().collect()
    }
    fn recover(&mut self) {
        self.inner = AudioHandler::new();
        self.recoveries += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use audiopus::{Application, Channels, SampleRate, coder::Encoder};
    use tsproto_packets::packets::{AudioData, CodecType, Direction, OutAudio};
    fn packet(id: u16, data: &[u8]) -> InAudioBuf {
        let out = OutAudio::new(&AudioData::S2C {
            id,
            codec: CodecType::OpusVoice,
            from: 7,
            data,
        });
        InAudioBuf::try_new(Direction::S2C, out.into_vec()).unwrap()
    }
    fn voice() -> Vec<u8> {
        let encoder = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip).unwrap();
        let pcm: Vec<_> = (0..960).map(|i| (i as f32 * 0.0576).sin() * 0.05).collect();
        let mut bytes = [0; 1275];
        let n = encoder.encode_float(&pcm, &mut bytes).unwrap();
        bytes[..n].to_vec()
    }
    #[test]
    fn damaged_decoder_queue_is_recovered_before_it_can_kill_capture() {
        let bytes = voice();
        // Reproduce the upstream cursor bug: a decode error after a partial read
        // leaves decoded_pos referring to data already drained on the previous call.
        let mut upstream = AudioHandler::<u16>::new();
        upstream.handle_packet(7, packet(0, &bytes)).unwrap();
        upstream.fill_buffer(&mut [0.; 960]);
        upstream.fill_buffer(&mut [0.; 960]);
        upstream.handle_packet(7, packet(1, &[131, 65])).unwrap();
        upstream.fill_buffer(&mut [0.; 960]);
        assert!(catch_unwind(AssertUnwindSafe(|| upstream.fill_buffer(&mut [0.; 960]))).is_err());

        let mut safe = Playback::new();
        safe.receive(7, packet(0, &bytes), 1.);
        safe.fill_buffer(&mut [0.; 960]);
        safe.fill_buffer(&mut [0.; 960]);
        safe.receive(7, packet(1, &[131, 65]), 1.);
        for _ in 0..4 {
            safe.fill_buffer(&mut [0.; 960]);
        }
        assert_eq!(safe.recoveries, 1);
        safe.receive(7, packet(20, &bytes), 1.);
        let mut out = [0.; 960];
        safe.fill_buffer(&mut out);
        assert!(out.iter().any(|s| s.abs() > 0.001));
    }
    #[test]
    fn loss_reordering_duplicates_and_sequence_wrap_do_not_stop_later_voice() {
        let bytes = voice();
        let mut playback = Playback::new();
        let mut peak = 0f32;
        for n in 0..300u16 {
            if n % 13 != 0 {
                playback.receive(7, packet(65500u16.wrapping_add(n), &bytes), 1.);
                if n % 7 == 0 {
                    playback.receive(7, packet(65500u16.wrapping_add(n), &bytes), 1.);
                }
                if n > 3 && n % 11 == 0 {
                    playback.receive(7, packet(65500u16.wrapping_add(n - 3), &bytes), 1.);
                }
            }
            for _ in 0..2 {
                let mut out = [0.; 960];
                playback.fill_buffer(&mut out);
                assert!(out.iter().all(|s| s.is_finite()));
                if n > 280 {
                    peak = peak.max(out.iter().copied().map(f32::abs).fold(0f32, f32::max));
                }
            }
        }
        assert!(playback.discarded > 0);
        assert!(peak > 0.001, "voice must resume after repeated packet loss");
    }
}
