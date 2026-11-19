use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    #[default]
    Disconnected,
    Connecting,
    Synchronizing,
    Connected,
    Reconnecting,
    Disconnecting,
    Failed,
}

// Passwords intentionally cannot be serialized or printed via Debug.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectRequest {
    pub address: String,
    pub nickname: String,
    pub password: Option<String>,
    pub channel: Option<String>,
    pub channel_password: Option<String>,
}

impl ConnectRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.address.trim().is_empty() || self.address.len() > 512 {
            return Err("服务器地址不能为空且不能超过 512 字节".into());
        }
        if self.nickname.trim().is_empty() || self.nickname.len() > 64 {
            return Err("昵称不能为空且不能超过 64 字节".into());
        }
        if self.address.chars().any(char::is_control) || self.nickname.chars().any(char::is_control)
        {
            return Err("地址和昵称不能包含控制字符".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    // TS3 channel IDs are u64. Strings avoid precision loss across JS IPC.
    pub id: String,
    pub parent_id: String,
    pub previous_id: String,
    pub name: String,
    pub codec: String,
    pub topic: Option<String>,
    pub description: Option<String>,
    pub has_password: bool,
    #[serde(default)]
    pub subscribed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Client {
    pub id: u16,
    pub channel_id: String,
    pub name: String,
    pub uid: Option<String>,
    pub input_muted: bool,
    pub output_muted: bool,
    pub away: Option<String>,
    pub description: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub generation: u32,
    pub sequence: u32,
    pub status: ConnectionStatus,
    pub server_name: Option<String>,
    pub own_client_id: Option<u16>,
    pub channels: Vec<Channel>,
    pub clients: Vec<Client>,
    pub last_error: Option<String>,
    pub notice: Option<String>,
    pub audio: AudioStatus,
    #[serde(default)]
    pub network: Option<NetworkStats>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkStats {
    pub round_trip_ms: f64,
    pub variation_ms: f64,
    pub inbound_voice_loss: f32,
    pub inbound_loss: f32,
    pub inbound_bytes: u64,
    pub outbound_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NoiseMode {
    Off,
    Standard,
    Rnnoise,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TalkMode {
    PushToTalk,
    VoiceActivation,
    Continuous,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct AudioSettings {
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub input_gain: f32,
    pub output_volume: f32,
    pub input_muted: bool,
    pub output_muted: bool,
    pub noise_mode: NoiseMode,
    pub echo_cancellation: bool,
    pub auto_gain: bool,
    pub talk_mode: TalkMode,
    #[serde(
        alias = "vadThresholdDb",
        deserialize_with = "deserialize_voice_sensitivity"
    )]
    pub voice_activation_sensitivity: f32,
    pub ptt_key: String,
}

const DEFAULT_VOICE_SENSITIVITY: f32 = 55.;

fn deserialize_voice_sensitivity<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<f32, D::Error> {
    let value = f32::deserialize(deserializer)?;
    // Existing profiles stored a negative dB threshold, which cannot be converted to a probability.
    Ok(if (-70.0..=-10.0).contains(&value) {
        DEFAULT_VOICE_SENSITIVITY
    } else {
        value
    })
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            input_device: None,
            output_device: None,
            input_gain: 1.,
            output_volume: 1.,
            input_muted: true,
            output_muted: false,
            noise_mode: NoiseMode::Standard,
            echo_cancellation: true,
            auto_gain: true,
            talk_mode: TalkMode::PushToTalk,
            voice_activation_sensitivity: DEFAULT_VOICE_SENSITIVITY,
            ptt_key: "F8".into(),
        }
    }
}

impl AudioSettings {
    pub fn changes_stream_processing(&self, other: &Self) -> bool {
        self.input_device != other.input_device
            || self.output_device != other.output_device
            || self.input_gain != other.input_gain
            || self.input_muted != other.input_muted
            || self.output_muted != other.output_muted
            || self.noise_mode != other.noise_mode
            || self.echo_cancellation != other.echo_cancellation
            || self.auto_gain != other.auto_gain
            || self.talk_mode != other.talk_mode
            || self.voice_activation_sensitivity != other.voice_activation_sensitivity
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.input_gain.is_finite()
            || !(0.0..=3.0).contains(&self.input_gain)
            || !self.output_volume.is_finite()
            || !(0.0..=2.0).contains(&self.output_volume)
            || !self.voice_activation_sensitivity.is_finite()
            || !(0.0..=100.0).contains(&self.voice_activation_sensitivity)
            || !["F8", "F9", "F10", "F11", "F12"].contains(&self.ptt_key.as_str())
        {
            return Err("音频参数超出支持范围".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioStatus {
    pub running: bool,
    pub input_ready: bool,
    pub output_ready: bool,
    pub input_level: f32,
    #[serde(default)]
    pub voice_probability: Option<f32>,
    pub raw_input_level: f32,
    #[serde(default)]
    pub input_clipped_samples: u64,
    #[serde(default)]
    pub capture_dropped_samples: u64,
    #[serde(default)]
    pub applied_input_gain: Option<f32>,
    #[serde(default)]
    pub echo_delay_ms: Option<u16>,
    pub output_level: f32,
    pub transmitting: bool,
    pub mic_test: bool,
    pub input_error: Option<String>,
    pub output_error: Option<String>,
    pub processing_error: Option<String>,
    pub speakers: Vec<u16>,
    pub received_packets: u64,
    pub can_receive: bool,
    pub can_send: bool,
    pub sent_packets: u64,
    pub discarded_packets: u64,
    pub playback_recoveries: u64,
    pub dsp_recoveries: u64,
    pub device_recoveries: u64,
    pub encoded_packets: u64,
    pub outgoing_drops: u64,
    pub recovery_notice: Option<String>,
    pub send_block_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub input: bool,
    pub output: bool,
}

impl Snapshot {
    pub fn transition(&mut self, status: ConnectionStatus) {
        self.status = status;
        self.sequence += 1;
    }

    pub fn clear_server(&mut self) {
        self.server_name = None;
        self.own_client_id = None;
        self.channels.clear();
        self.clients.clear();
        self.network = None;
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    Presence {
        generation: u32,
        name: String,
        channel: String,
        joined: bool,
    },
    Snapshot {
        snapshot: Box<Snapshot>,
    },
    Message {
        generation: u32,
        sender: String,
        sender_id: u16,
        sender_uid: Option<String>,
        channel_id: Option<String>,
        target: String,
        text: String,
    },
    CommandResult {
        request_id: u32,
        error: Option<String>,
    },
}

/// UI synchronization helper: rejects snapshots from a previous connection or older updates.
pub fn is_newer(incoming: &Snapshot, current: &Snapshot) -> bool {
    (incoming.generation, incoming.sequence) > (current.generation, current.sequence)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_cannot_be_overwritten_by_old_events() {
        let current = Snapshot {
            generation: 2,
            sequence: 1,
            ..Default::default()
        };
        let stale = Snapshot {
            generation: 1,
            sequence: 500,
            ..Default::default()
        };
        assert!(!is_newer(&stale, &current));
        let next = Snapshot {
            generation: 2,
            sequence: 2,
            ..Default::default()
        };
        assert!(is_newer(&next, &current));
        assert!(!is_newer(&current, &current));
    }

    #[test]
    fn rejects_terminal_control_characters_in_connection_fields() {
        let request = ConnectRequest {
            address: "localhost\n".into(),
            nickname: "CongSpeak".into(),
            password: None,
            channel: None,
            channel_password: None,
        };
        assert!(request.validate().is_err());
    }
}

#[derive(Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientAction {
    Join {
        channel_id: String,
        password: Option<String>,
    },
    Rename {
        name: String,
    },
    Away {
        message: Option<String>,
    },
    Subscribe {
        enabled: bool,
    },
    Poke {
        client_id: u16,
        message: String,
    },
    Move {
        client_id: u16,
        channel_id: String,
        password: Option<String>,
    },
    Kick {
        client_id: u16,
        from_server: bool,
        reason: String,
    },
    EditChannel {
        channel_id: String,
        name: String,
        topic: String,
        description: Option<String>,
    },
    CreateChannel {
        parent_id: String,
        name: String,
        topic: String,
        description: String,
        password: Option<String>,
        lifetime: ChannelLifetime,
    },
    MoveChannel {
        channel_id: String,
        parent_id: String,
        previous_id: Option<String>,
    },
    DeleteChannel {
        channel_id: String,
    },
    SubscribeChannel {
        channel_id: String,
        enabled: bool,
    },
    ChannelPassword {
        channel_id: String,
        password: Option<String>,
    },
    ClientDescription {
        description: String,
    },
    RequestTalk {
        message: Option<String>,
    },
    GrantTalk {
        client_id: u16,
        granted: bool,
    },
    BanClient {
        client_id: u16,
        minutes: u32,
        reason: String,
    },
    RemoveBan {
        generation: u32,
        ban_id: u32,
    },
    Complaint {
        client_id: u16,
        uid: String,
        message: String,
    },
    RedeemKey {
        key: String,
    },
}
impl ClientAction {
    pub fn target_client(&self) -> Option<u16> {
        match self {
            Self::Poke { client_id, .. }
            | Self::Move { client_id, .. }
            | Self::Kick { client_id, .. }
            | Self::GrantTalk { client_id, .. }
            | Self::BanClient { client_id, .. }
            | Self::Complaint { client_id, .. } => Some(*client_id),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatTarget {
    Channel,
    Server,
    Client { client_id: u16 },
}
impl ChatTarget {
    pub fn key(&self) -> String {
        match self {
            Self::Channel => "channel".into(),
            Self::Server => "server".into(),
            Self::Client { client_id } => format!("client:{client_id}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelLifetime {
    Temporary,
    SemiPermanent,
    Permanent,
}
