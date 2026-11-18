use crate::IdentityMode;
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::Mutex,
};
use tsclientlib::Identity;
use tsproto_types::crypto::EccKeyPrivP256;
use uuid::Uuid;

const MAX_INI_BYTES: u64 = 64 * 1024;
static DEFAULT_LOAD: Mutex<()> = Mutex::new(());

pub struct Info {
    pub uid: String,
    pub level: u8,
}

fn credential(mode: IdentityMode) -> Result<keyring::Entry, String> {
    let account = match mode {
        IdentityMode::SystemCredential => "default-identity".to_owned(),
        IdentityMode::Named(id) => format!("identity-{id}"),
        _ => return Err("临时身份没有系统凭据".into()),
    };
    keyring::Entry::new("app.congspeak.desktop", &account)
        .map_err(|_| "无法打开系统凭据存储".into())
}

pub(crate) fn load(mode: IdentityMode) -> Result<Identity, String> {
    #[cfg(test)]
    if matches!(mode, IdentityMode::DelayedTest) {
        std::thread::sleep(std::time::Duration::from_millis(500));
        return Ok(Identity::create());
    }
    if matches!(mode, IdentityMode::Ephemeral) {
        return Ok(Identity::create());
    }
    // Connection preparation can outlive a cancelled request. Serialize lazy creation with
    // a later lookup from the identity manager so they cannot generate different default keys.
    let _default_load = matches!(mode, IdentityMode::SystemCredential).then(|| {
        DEFAULT_LOAD
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    });
    let entry = credential(mode)?;
    match entry.get_password() {
        Ok(secret) => serde_json::from_str(&secret)
            .map_err(|_| "系统凭据中的身份无法读取，请勿删除原身份".into()),
        Err(keyring::Error::NoEntry) if matches!(mode, IdentityMode::SystemCredential) => {
            let identity = Identity::create();
            save(mode, &identity)?;
            Ok(identity)
        }
        Err(keyring::Error::NoEntry) => {
            Err("此身份在系统凭据中不存在，请重新导入或选择身份".into())
        }
        Err(_) => Err("无法读取系统凭据，请检查授权".into()),
    }
}

pub(crate) fn save(mode: IdentityMode, identity: &Identity) -> Result<(), String> {
    let secret = serde_json::to_string(identity).map_err(|_| "无法编码身份".to_string())?;
    credential(mode)?
        .set_password(&secret)
        .map_err(|_| "无法保存身份到系统凭据".into())
}

pub fn info(mode: IdentityMode) -> Result<Info, String> {
    let identity = load(mode)?;
    Ok(Info {
        uid: identity.key().to_pub().get_uid(),
        level: identity.level(),
    })
}

pub fn create() -> Result<Uuid, String> {
    store_new(Identity::create())
}

fn store_new(identity: Identity) -> Result<Uuid, String> {
    let id = Uuid::new_v4();
    save(IdentityMode::Named(id), &identity)?;
    Ok(id)
}

pub fn remove(id: Uuid) -> Result<(), String> {
    match credential(IdentityMode::Named(id))?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err("无法删除系统凭据中的身份".into()),
    }
}

pub fn import_ini(path: &Path) -> Result<Uuid, String> {
    if !path.is_absolute() {
        return Err("请输入身份文件的本地完整路径".into());
    }
    if !path
        .metadata()
        .map_err(|e| format!("无法读取身份文件：{e}"))?
        .is_file()
    {
        return Err("请选择普通身份文件".into());
    }
    let file = File::open(path).map_err(|e| format!("无法打开身份文件：{e}"))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("请选择普通身份文件".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_INI_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("无法读取身份文件：{e}"))?;
    if bytes.len() as u64 > MAX_INI_BYTES {
        return Err("身份文件过大".into());
    }
    let source = std::str::from_utf8(&bytes).map_err(|_| "身份文件必须使用 UTF-8")?;
    store_new(parse_ini(source)?)
}

fn parse_ini(source: &str) -> Result<Identity, String> {
    let mut in_identity = false;
    let mut encoded = None;
    for line in source.trim_start_matches('\u{feff}').lines().map(str::trim) {
        if line.starts_with('[') {
            in_identity = line.eq_ignore_ascii_case("[Identity]");
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if in_identity
            && key.trim().eq_ignore_ascii_case("identity")
            && encoded.replace(value.trim()).is_some()
        {
            return Err("文件包含多个身份，请分别导出".into());
        }
    }
    let encoded = encoded
        .ok_or("INI 文件缺少 identity 字段")?
        .trim_matches('"');
    let (counter, key) = encoded.split_once('V').ok_or("身份格式无效")?;
    let counter = counter.parse().map_err(|_| "身份安全等级计数无效")?;
    let key = EccKeyPrivP256::from_ts_obfuscated(key).map_err(|_| "TS3 身份私钥无法解码")?;
    Ok(Identity::new(key, counter))
}

pub fn export_ini(mode: IdentityMode, path: &Path, name: &str) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("请输入身份文件的本地完整路径".into());
    }
    if name.len() > 120 || name.chars().any(char::is_control) {
        return Err("身份名称无效".into());
    }
    let parent = path.parent().ok_or("无法确定导出目录")?;
    let identity = load(mode)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temp.write_all(encode_ini(&identity, name).as_bytes())
        .map_err(|e| e.to_string())?;
    temp.as_file().sync_all().map_err(|e| e.to_string())?;
    temp.persist_noclobber(path)
        .map_err(|e| format!("无法保存身份文件：{}", e.error))?;
    Ok(())
}

fn encode_ini(identity: &Identity, name: &str) -> String {
    let name = name.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "[Identity]\r\nid=\"{name}\"\r\nidentity=\"{}V{}\"\r\nnickname=CongSpeak\r\n",
        identity.counter(),
        identity.key().to_ts_obfuscated()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ts3_ini_round_trip_preserves_uid_and_counter() {
        let original = Identity::create();
        let ini = encode_ini(&original, "语音身份");
        let imported = parse_ini(&format!("\u{feff}{ini}")).unwrap();
        assert_eq!(
            imported.key().to_pub().get_uid(),
            original.key().to_pub().get_uid()
        );
        assert_eq!(imported.counter(), original.counter());
        assert_eq!(imported.level(), original.level());
        assert!(parse_ini(&format!("{ini}{ini}")).is_err());
        assert!(parse_ini("[Identity]\nidentity=\"8Vbad\"").is_err());
    }
}
