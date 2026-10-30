use serde::{Deserialize, Serialize};
use std::path::Path;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
use winreg::RegKey;

const REGISTRY_PATH: &str = r"Software\LocalPrivacyScreenRust";
const REGISTRY_VALUE: &str = "SettingsJson";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hotkey {
    pub modifiers: u32,
    pub virtual_key: u32,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub hotkey: Hotkey,
    pub background_path: String,
    pub fit: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: Hotkey {
                modifiers: 2 | 4,
                virtual_key: b'P' as u32,
                label: "Ctrl+Shift+P".into(),
            },
            background_path: String::new(),
            fit: "cover".into(),
        }
    }
}

pub fn load_settings() -> Result<Settings, String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = match hkcu.open_subkey_with_flags(REGISTRY_PATH, KEY_READ) {
        Ok(key) => key,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(error) => return Err(format!("读取设置失败: {error}")),
    };
    match key.get_value::<String, _>(REGISTRY_VALUE) {
        Ok(json) => serde_json::from_str(&json).map_err(|error| format!("设置格式无效: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(error) => Err(format!("读取设置失败: {error}")),
    }
}

pub fn save_settings(settings: &Settings) -> Result<(), String> {
    validate_settings(settings)?;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(REGISTRY_PATH)
        .map_err(|error| format!("创建设置失败: {error}"))?;
    let json = serde_json::to_string(settings).map_err(|error| format!("序列化设置失败: {error}"))?;
    key.set_value(REGISTRY_VALUE, &json)
        .map_err(|error| format!("保存设置失败: {error}"))?;
    Ok(())
}

pub fn validate_settings(settings: &Settings) -> Result<(), String> {
    let valid_modifiers = settings.hotkey.modifiers & !(1 | 2 | 4 | 8) == 0;
    if settings.hotkey.modifiers == 0 || !valid_modifiers {
        return Err("快捷键至少需要一个 Ctrl、Alt、Shift 或 Win 修饰键".into());
    }
    if settings.hotkey.virtual_key == 0 || settings.hotkey.virtual_key > 0xFF {
        return Err("快捷键按键无效".into());
    }
    if !matches!(settings.fit.as_str(), "cover" | "contain" | "stretch") {
        return Err("铺图方式无效".into());
    }
    if !settings.background_path.trim().is_empty() {
        let path = Path::new(&settings.background_path);
        if !path.is_file() {
            return Err("背景图片不存在".into());
        }
    }
    Ok(())
}

pub fn clear_settings() -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    match hkcu.delete_subkey_all(REGISTRY_PATH) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("清除设置失败: {error}")),
    }
}
