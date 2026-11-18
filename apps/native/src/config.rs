use congspeak_core::AudioSettings;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Write, path::PathBuf};
use uuid::Uuid;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Bookmark {
    pub name: String,
    pub address: String,
    pub nickname: String,
    pub channel: String,
    pub group: String,
    pub identity: Option<Uuid>,
}
impl Bookmark {
    pub fn validate(&self) -> Result<(), String> {
        for (value, limit) in [
            (&self.name, 256),
            (&self.address, 512),
            (&self.nickname, 64),
            (&self.channel, 512),
            (&self.group, 256),
        ] {
            if value.len() > limit || value.chars().any(char::is_control) {
                return Err("收藏字段过长或含控制字符".into());
            }
        }
        if self.name.trim().is_empty() || self.address.trim().is_empty() {
            return Err("收藏名称和地址不能为空".into());
        }
        Ok(())
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Contact {
    pub name: String,
    pub note: String,
    pub ignored: bool,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Preferences {
    pub address: String,
    pub nickname: String,
    pub channel: String,
    pub bookmarks: Vec<Bookmark>,
    pub contacts: BTreeMap<String, Contact>,
    pub identities: BTreeMap<Uuid, String>,
    pub identity: Option<Uuid>,
    pub light: bool,
    pub mute_presence: bool,
    pub occupied_only: bool,
}
pub fn directory() -> Result<PathBuf, String> {
    dirs::config_dir()
        .map(|p| p.join("app.congspeak.desktop"))
        .ok_or("无法确定配置目录".into())
}
pub fn load() -> (Preferences, AudioSettings, Option<String>) {
    let Ok(dir) = directory() else {
        return (
            Preferences::default(),
            AudioSettings::default(),
            Some("无法读取配置目录".into()),
        );
    };
    let (preferences, preference_issue) = match std::fs::read(dir.join("native.json")) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(value) => (value, None),
            Err(e) => (
                Preferences::default(),
                Some(format!("读取本地配置失败：{e}")),
            ),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Preferences::default(), None),
        Err(e) => (
            Preferences::default(),
            Some(format!("读取本地配置失败：{e}")),
        ),
    };
    let (mut audio, issue) = match std::fs::read(dir.join("audio.json")) {
        Ok(bytes) => match serde_json::from_slice::<AudioSettings>(&bytes) {
            Ok(s) if s.validate().is_ok() => (s, None),
            _ => (
                AudioSettings::default(),
                Some("音频设置无效，已使用默认值".into()),
            ),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (AudioSettings::default(), None),
        Err(e) => (
            AudioSettings::default(),
            Some(format!("读取音频设置失败：{e}")),
        ),
    };
    audio.input_muted = true;
    let issue = match (preference_issue, issue) {
        (Some(preferences), Some(audio)) => Some(format!("{preferences}。{audio}")),
        (a, b) => a.or(b),
    };
    (preferences, audio, issue)
}
pub fn save<T: Serialize>(filename: &str, value: &T) -> Result<(), String> {
    let dir = directory()?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let mut temp = tempfile::NamedTempFile::new_in(&dir).map_err(|e| e.to_string())?;
    temp.write_all(&bytes).map_err(|e| e.to_string())?;
    temp.as_file().sync_all().map_err(|e| e.to_string())?;
    temp.persist(dir.join(filename))
        .map_err(|error| error.error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_voice_profiles_keep_the_mode_and_pick_default_sensitivity() {
        let settings: AudioSettings =
            serde_json::from_str(r#"{"talkMode":"voice_activation","vadThresholdDb":-30}"#)
                .unwrap();
        assert_eq!(
            settings.talk_mode,
            congspeak_core::TalkMode::VoiceActivation
        );
        assert_eq!(
            settings.voice_activation_sensitivity,
            AudioSettings::default().voice_activation_sensitivity
        );
        let encoded = serde_json::to_string(&settings).unwrap();
        assert!(encoded.contains("voiceActivationSensitivity"));
        assert!(!encoded.contains("vadThresholdDb"));
        assert_eq!(
            serde_json::from_str::<AudioSettings>(&encoded).unwrap(),
            settings
        );
    }
}
