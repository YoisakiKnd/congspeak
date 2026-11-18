use crate::config;
use congspeak_core::{AudioDevice, AudioSettings, SessionEvent, Snapshot};
use congspeak_teamspeak::{IdentityMode, Session, identities};
use eframe::egui;
use std::{
    future::Future,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;
use uuid::Uuid;

pub enum IdentityChange {
    Stored(Uuid, String),
    Removed(Uuid),
    Info(Option<Uuid>, identities::Info),
    Exported,
}

pub enum Event {
    Protocol(SessionEvent),
    Error(String),
    Settings(Result<AudioSettings, String>),
    Devices(Result<Vec<AudioDevice>, String>),
    Identity(Result<IdentityChange, String>),
}
pub struct Backend {
    pub session: Session,
    runtime: Option<tokio::runtime::Runtime>,
    pub events: mpsc::Receiver<Event>,
    sender: mpsc::Sender<Event>,
    ctx: egui::Context,
    persistence: Arc<tokio::sync::Mutex<()>>,
    latest_preferences: Arc<Mutex<Option<config::Preferences>>>,
}
impl Backend {
    pub fn new(ctx: &egui::Context, settings: AudioSettings) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("Tokio runtime");
        let session = {
            let _guard = runtime.enter();
            Session::start_with_audio(IdentityMode::SystemCredential, settings)
        };
        let (sender, events) = mpsc::channel(128);
        let mut incoming = session.subscribe();
        let (tx, ctx) = (sender.clone(), ctx.clone());
        let notify = ctx.clone();
        runtime.spawn(async move {
            let mut previous = 0u64;
            loop {
                match incoming.recv().await {
                    Ok(SessionEvent::Snapshot { snapshot }) => {
                        let signature = signature(&snapshot);
                        if signature != previous {
                            previous = signature;
                            notify.request_repaint();
                        }
                    }
                    Ok(event) => {
                        notify.request_repaint();
                        if tx.send(Event::Protocol(event)).await.is_err() {
                            break;
                        }
                        notify.request_repaint();
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        notify.request_repaint();
                        let _ = tx
                            .send(Event::Error(format!(
                                "界面事件积压，{count} 条事件未能处理"
                            )))
                            .await;
                        notify.request_repaint();
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            session,
            runtime: Some(runtime),
            events,
            sender,
            ctx,
            persistence: Arc::new(tokio::sync::Mutex::new(())),
            latest_preferences: Arc::new(Mutex::new(None)),
        }
    }
    pub fn runtime(&self) -> &tokio::runtime::Runtime {
        self.runtime.as_ref().expect("live runtime")
    }
    pub fn task(&self, future: impl Future<Output = Result<(), String>> + Send + 'static) {
        let (tx, ctx) = (self.sender.clone(), self.ctx.clone());
        self.runtime().spawn(async move {
            if let Err(error) = future.await {
                ctx.request_repaint();
                let _ = tx.send(Event::Error(error)).await;
                ctx.request_repaint();
            }
        });
    }
    pub fn apply(&self, settings: AudioSettings) {
        self.configure(settings, None);
    }
    pub fn request(
        &self,
        id: u32,
        future: impl Future<Output = Result<(), String>> + Send + 'static,
    ) {
        let (tx, ctx) = (self.sender.clone(), self.ctx.clone());
        self.runtime().spawn(async move {
            let error = future.await.err();
            let _ = tx
                .send(Event::Protocol(SessionEvent::CommandResult {
                    request_id: id,
                    error,
                }))
                .await;
            ctx.request_repaint();
        });
    }
    pub fn apply_test(&self, settings: AudioSettings, speaker: bool) {
        self.configure(settings, Some(speaker));
    }
    fn configure(&self, settings: AudioSettings, test: Option<bool>) {
        let (session, tx, ctx) = (self.session.clone(), self.sender.clone(), self.ctx.clone());
        self.runtime().spawn(async move {
            let result = async {
                settings.validate()?;
                session.set_audio_settings(settings.clone()).await?;
                let saved = settings.clone();
                let result =
                    tokio::task::spawn_blocking(move || config::save("audio.json", &saved))
                        .await
                        .map_err(|e| e.to_string())?;
                if let Err(e) = result {
                    ctx.request_repaint();
                    let _ = tx
                        .send(Event::Error(format!("音频已应用，保存失败：{e}")))
                        .await;
                }
                if let Some(speaker) = test {
                    let result = if speaker {
                        session.test_speaker().await
                    } else {
                        session.mic_test(true).await
                    };
                    if let Err(error) = result {
                        ctx.request_repaint();
                        let _ = tx.send(Event::Error(error)).await;
                    }
                }
                Ok(settings)
            }
            .await;
            ctx.request_repaint();
            let _ = tx.send(Event::Settings(result)).await;
            ctx.request_repaint();
        });
    }
    pub fn devices(&self) {
        let (tx, ctx) = (self.sender.clone(), self.ctx.clone());
        self.runtime().spawn(async move {
            let result = tokio::task::spawn_blocking(congspeak_audio::runtime::devices)
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            ctx.request_repaint();
            let _ = tx.send(Event::Devices(result)).await;
            ctx.request_repaint();
        });
    }
    pub fn identity(
        &self,
        operation: impl FnOnce() -> Result<IdentityChange, String> + Send + 'static,
    ) {
        let (tx, ctx) = (self.sender.clone(), self.ctx.clone());
        self.runtime().spawn(async move {
            let result = tokio::task::spawn_blocking(operation)
                .await
                .unwrap_or_else(|_| Err("身份管理任务异常".into()));
            let _ = tx.send(Event::Identity(result)).await;
            ctx.request_repaint();
        });
    }
    pub fn save_preferences(&self, preferences: config::Preferences) {
        *self
            .latest_preferences
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(preferences);
        self.task(self.flush_preferences());
    }

    pub fn flush_preferences(&self) -> impl Future<Output = Result<(), String>> + Send + 'static {
        let (latest, persistence) = (self.latest_preferences.clone(), self.persistence.clone());
        async move {
            let _guard = persistence.lock().await;
            let value = latest.lock().unwrap_or_else(|e| e.into_inner()).take();
            let Some(value) = value else {
                return Ok(());
            };
            tokio::task::spawn_blocking(move || config::save("native.json", &value))
                .await
                .map_err(|e| e.to_string())?
        }
    }
}
fn signature(s: &Snapshot) -> u64 {
    let mut hash = std::hash::DefaultHasher::new();
    format!("{:?}", s.status).hash(&mut hash);
    s.generation.hash(&mut hash);
    s.server_name.hash(&mut hash);
    s.own_client_id.hash(&mut hash);
    s.last_error.hash(&mut hash);
    s.notice.hash(&mut hash);
    for c in &s.channels {
        (
            &c.id,
            &c.parent_id,
            &c.previous_id,
            &c.name,
            &c.topic,
            &c.description,
            c.has_password,
            c.subscribed,
        )
            .hash(&mut hash);
    }
    for c in &s.clients {
        (
            c.id,
            &c.channel_id,
            &c.name,
            c.input_muted,
            c.output_muted,
            &c.away,
            &c.description,
        )
            .hash(&mut hash);
    }
    let a = &s.audio;
    (
        a.input_ready,
        a.output_ready,
        a.transmitting,
        a.mic_test,
        &a.input_error,
        &a.output_error,
        &a.processing_error,
        &a.speakers,
        a.can_send,
        a.can_receive,
        &a.recovery_notice,
    )
        .hash(&mut hash);
    hash.finish()
}

impl Drop for Backend {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unchanged_heartbeat_does_not_wake_idle_window_but_permissions_do() {
        let mut state = Snapshot::default();
        let original = signature(&state);
        state.sequence += 1;
        state.audio.sent_packets += 50;
        state.audio.raw_input_level = 0.01;
        assert_eq!(signature(&state), original);
        state.audio.can_send = true;
        assert_ne!(signature(&state), original);
    }
}
