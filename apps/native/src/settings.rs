use crate::app::{App, talk_name};
use congspeak_core::{AudioSettings, NoiseMode, TalkMode};
use eframe::egui;

impl App {
    pub fn settings(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut apply = false;
        let mut speaker = false;
        let mut microphone = false;
        egui::Window::new("音频设置")
            .open(&mut open)
            .default_width(500.)
            .default_height(520.)
            .vscroll(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("设备与音量");
                    if ui.small_button("刷新设备").clicked() {
                        self.backend.devices();
                    }
                });
                egui::Grid::new("audio-device-grid")
                    .num_columns(2)
                    .spacing([12., 8.])
                    .show(ui, |ui| {
                        for input in [true, false] {
                            let selected = if input {
                                &mut self.draft.input_device
                            } else {
                                &mut self.draft.output_device
                            };
                            let text = selected
                                .as_ref()
                                .and_then(|id| self.devices.iter().find(|d| &d.id == id))
                                .map(|d| d.name.as_str())
                                .unwrap_or(if selected.is_some() {
                                    "已选设备不可用"
                                } else {
                                    "跟随系统默认"
                                });
                            ui.label(if input {
                                "麦克风"
                            } else {
                                "扬声器/耳机"
                            });
                            egui::ComboBox::from_id_salt(("device", input))
                                .selected_text(text)
                                .width(320.)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(selected, None, "跟随系统默认");
                                    for device in self
                                        .devices
                                        .iter()
                                        .filter(|d| if input { d.input } else { d.output })
                                    {
                                        ui.selectable_value(
                                            selected,
                                            Some(device.id.clone()),
                                            &device.name,
                                        );
                                    }
                                });
                            ui.end_row();
                        }
                    });
                ui.add(egui::Slider::new(&mut self.draft.input_gain, 0.0..=3.0).text("麦克风增益"));
                ui.add(
                    egui::Slider::new(&mut self.draft.output_volume, 0.0..=2.0).text("播放音量"),
                );

                let a = &self.snapshot.audio;
                for (label, value) in [
                    ("原始麦克风", a.raw_input_level),
                    ("处理后声音", a.input_level),
                    ("播放电平", a.output_level),
                ] {
                    let db = 20. * value.max(1e-6).log10();
                    ui.add(
                        egui::ProgressBar::new(((db + 60.) / 60.).clamp(0., 1.))
                            .text(format!("{label} {db:.1} dB")),
                    );
                }
                ui.horizontal(|ui| {
                    let can_test = !self.settings_busy && !self.draft.output_muted;
                    if ui
                        .add_enabled(can_test, egui::Button::new("播放测试音"))
                        .clicked()
                    {
                        speaker = true;
                    }
                    if ui
                        .add_enabled(
                            can_test,
                            egui::Button::new(if a.mic_test {
                                "停止试听"
                            } else {
                                "麦克风本机试听"
                            }),
                        )
                        .clicked()
                    {
                        microphone = true;
                    }
                    ui.weak("本机试听期间暂停网络发言");
                });

                ui.separator();
                ui.strong("声音处理与发言方式");
                egui::Grid::new("audio-dsp-grid")
                    .num_columns(2)
                    .spacing([12., 8.])
                    .show(ui, |ui| {
                        ui.label("降噪模式");
                        egui::ComboBox::from_id_salt("noise-mode")
                            .selected_text(match self.draft.noise_mode {
                                NoiseMode::Off => "关闭降噪",
                                NoiseMode::Standard => "标准降噪 · WebRTC",
                                NoiseMode::Rnnoise => "AI 降噪 · RNNoise",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.draft.noise_mode,
                                    NoiseMode::Off,
                                    "关闭降噪",
                                );
                                ui.selectable_value(
                                    &mut self.draft.noise_mode,
                                    NoiseMode::Standard,
                                    "标准降噪 · WebRTC",
                                );
                                ui.selectable_value(
                                    &mut self.draft.noise_mode,
                                    NoiseMode::Rnnoise,
                                    "AI 降噪 · RNNoise",
                                );
                            });
                        ui.end_row();

                        ui.label("增强选项");
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.draft.echo_cancellation, "回声消除");
                            ui.checkbox(&mut self.draft.auto_gain, "自动增益");
                        });
                        ui.end_row();

                        ui.label("发送模式");
                        egui::ComboBox::from_id_salt("settings-talk")
                            .selected_text(talk_name(&self.draft.talk_mode))
                            .show_ui(ui, |ui| {
                                for mode in [
                                    TalkMode::PushToTalk,
                                    TalkMode::VoiceActivation,
                                    TalkMode::Continuous,
                                ] {
                                    let text = talk_name(&mode);
                                    ui.selectable_value(&mut self.draft.talk_mode, mode, text);
                                }
                            });
                        ui.end_row();

                        if self.draft.talk_mode == TalkMode::PushToTalk {
                            ui.label("全局快捷键");
                            egui::ComboBox::from_id_salt("ptt-key")
                                .selected_text(&self.draft.ptt_key)
                                .show_ui(ui, |ui| {
                                    for key in ["F8", "F9", "F10", "F11", "F12"] {
                                        ui.selectable_value(
                                            &mut self.draft.ptt_key,
                                            key.into(),
                                            key,
                                        );
                                    }
                                });
                            ui.end_row();
                        }
                    });
                if self.draft.talk_mode == TalkMode::VoiceActivation {
                    ui.add(
                        egui::Slider::new(
                            &mut self.draft.voice_activation_sensitivity,
                            0.0..=100.0,
                        )
                        .step_by(1.)
                        .text("人声检测灵敏度"),
                    );
                    if let Some(probability) = a.voice_probability {
                        ui.add(
                            egui::ProgressBar::new(probability)
                                .text(format!("人声置信度 {:.0}%", probability * 100.)),
                        );
                    }
                }

                ui.separator();
                let has_audio_error = a.input_error.is_some()
                    || a.output_error.is_some()
                    || a.processing_error.is_some();
                egui::CollapsingHeader::new("运行状态与恢复诊断")
                    .default_open(has_audio_error)
                    .show(ui, |ui| {
                        ui.label(a.send_block_reason.as_deref().unwrap_or("声音链路已就绪"));
                        for error in [&a.input_error, &a.output_error, &a.processing_error]
                            .into_iter()
                            .flatten()
                        {
                            ui.colored_label(egui::Color32::LIGHT_RED, error);
                        }
                        ui.label(format!(
                            "编码 {} · 提交发送 {} · 接收 {} · 队列丢弃 {}",
                            a.encoded_packets, a.sent_packets, a.received_packets, a.outgoing_drops
                        ));
                        ui.label(format!(
                            "恢复：DSP {} · 设备 {} · 接收 {}",
                            a.dsp_recoveries, a.device_recoveries, a.playback_recoveries
                        ));
                        ui.horizontal(|ui| {
                            if ui.button("重启音频（保留连接）").clicked() {
                                let session = self.backend.session.clone();
                                self.call(async move { session.restart_audio().await });
                            }
                            if ui.button("复制诊断 JSON").clicked() {
                                let report = serde_json::json!({
                                    "version": env!("CARGO_PKG_VERSION"),
                                    "audio": a,
                                    "history": self.diagnostics,
                                    "settings": audio_report(&self.audio),
                                });
                                ctx.copy_text(
                                    serde_json::to_string_pretty(&report).unwrap_or_default(),
                                );
                            }
                        });
                    });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !self.settings_busy,
                            egui::Button::new(if self.settings_busy {
                                "正在应用…"
                            } else {
                                "应用并保存"
                            }),
                        )
                        .clicked()
                    {
                        apply = true;
                    }
                    ui.weak("启动时默认保持麦克风静音。");
                });
            });
        self.settings_open = open;
        if !open {
            let session = self.backend.session.clone();
            self.call(async move { session.mic_test(false).await });
        }
        if apply {
            self.apply_audio(self.draft.clone());
        }
        if speaker || microphone {
            if microphone && self.snapshot.audio.mic_test {
                let session = self.backend.session.clone();
                self.call(async move { session.mic_test(false).await });
            } else if !self.settings_busy {
                self.settings_busy = true;
                self.backend.apply_test(self.draft.clone(), speaker);
            }
        }
    }
}

fn audio_report(a: &AudioSettings) -> serde_json::Value {
    serde_json::json!({
        "talkMode": a.talk_mode,
        "inputMuted": a.input_muted,
        "outputMuted": a.output_muted,
        "inputGain": a.input_gain,
        "noiseMode": a.noise_mode,
        "echoCancellation": a.echo_cancellation,
        "autoGain": a.auto_gain,
        "voiceActivationSensitivity": a.voice_activation_sensitivity,
    })
}
