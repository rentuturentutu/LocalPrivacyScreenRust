#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod logging;
mod privacy;
mod settings;

use base64::Engine;
use image::ImageFormat;
use serde::Serialize;
use std::io::Cursor;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, WebviewWindow};

use settings::{clear_settings, load_settings, save_settings, validate_settings, Settings};

static PRIVACY_RUNNING: AtomicBool = AtomicBool::new(false);
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PrivacyStatus {
    running: bool,
    error: Option<String>,
}

#[tauri::command]
fn get_settings() -> Result<Settings, String> {
    let result = load_settings();
    if let Err(error) = &result {
        logging::error(format!("读取设置失败: {error}"));
    }
    result
}

#[tauri::command]
fn persist_settings(settings: Settings) -> Result<(), String> {
    let result = save_settings(&settings);
    if let Err(error) = &result {
        logging::error(format!("保存设置失败: {error}"));
    }
    result
}

#[tauri::command]
fn reset_settings() -> Result<Settings, String> {
    clear_settings().map_err(|error| {
        logging::error(format!("恢复默认设置失败: {error}"));
        error
    })?;
    logging::info("settings-reset");
    Ok(Settings::default())
}

#[tauri::command]
async fn pick_background() -> Result<Option<String>, String> {
    let result = tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("选择隐私屏背景图片")
            .add_filter("图片", &["png", "jpg", "jpeg", "bmp", "gif"])
            .pick_file()
            .map(|path| path.to_string_lossy().to_string())
    })
    .await
    .map_err(|error| format!("打开图片选择窗口失败: {error}"));
    match &result {
        Ok(Some(_)) => logging::info("background-selected"),
        Ok(None) => logging::info("background-selection-cancelled"),
        Err(error) => logging::error(format!("背景文件选择失败: {error}")),
    }
    result
}

#[tauri::command]
async fn preview_background(path: String) -> Result<String, String> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        let image = image::open(&path).map_err(|error| format!("读取图片失败: {error}"))?;
        let thumbnail = image.thumbnail(900, 560);
        let mut bytes = Cursor::new(Vec::new());
        thumbnail
            .write_to(&mut bytes, ImageFormat::Png)
            .map_err(|error| format!("生成预览失败: {error}"))?;
        Ok(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
        ))
    })
    .await
    .map_err(|error| format!("生成预览失败: {error}"))?;
    if let Err(error) = &result {
        logging::error(format!("background-preview-failed: {error}"));
    }
    result
}

#[tauri::command]
fn privacy_status() -> PrivacyStatus {
    PrivacyStatus {
        running: PRIVACY_RUNNING.load(Ordering::SeqCst),
        error: LAST_ERROR.lock().ok().and_then(|error| error.clone()),
    }
}

#[tauri::command]
fn start_privacy(window: WebviewWindow, settings: Settings) -> Result<(), String> {
    logging::info(format!(
        "privacy-start-requested hotkey={} fit={} background={}",
        settings.hotkey.label,
        settings.fit,
        if settings.background_path.trim().is_empty() { "none" } else { "custom" }
    ));
    validate_settings(&settings).map_err(|error| {
        logging::error(format!("privacy-start-validation-failed: {error}"));
        error
    })?;
    save_settings(&settings).map_err(|error| {
        logging::error(format!("privacy-start-settings-save-failed: {error}"));
        error
    })?;
    if PRIVACY_RUNNING.swap(true, Ordering::SeqCst) {
        logging::warn("privacy-start-rejected: already-running");
        return Err("隐私屏已经在运行".into());
    }
    if let Ok(mut error) = LAST_ERROR.lock() {
        *error = None;
    }
    if let Err(error) = window.hide() {
        PRIVACY_RUNNING.store(false, Ordering::SeqCst);
        logging::error(format!("settings-window-hide-failed: {error}"));
        return Err(format!("隐藏设置窗口失败: {error}"));
    }

    std::thread::spawn(move || {
        let result = privacy::run_privacy(&settings);
        match &result {
            Ok(()) => logging::info("privacy-worker-finished: ok"),
            Err(error) => logging::error(format!("privacy-worker-finished: {error}")),
        }
        if let Ok(mut error) = LAST_ERROR.lock() {
            *error = result.err();
        }
        PRIVACY_RUNNING.store(false, Ordering::SeqCst);
        let _ = window.show();
        let _ = window.set_focus();
    });
    Ok(())
}

#[tauri::command]
fn minimize_window(window: WebviewWindow) {
    let _ = window.minimize();
}

#[tauri::command]
fn close_app(app: AppHandle) {
    logging::info("application-close-requested");
    app.exit(0);
}

#[tauri::command]
fn open_log() -> Result<(), String> {
    let path = logging::path().ok_or_else(|| "日志文件尚未初始化".to_string())?;
    Command::new("explorer.exe")
        .arg("/select,")
        .arg(path)
        .spawn()
        .map_err(|error| format!("打开日志失败: {error}"))?;
    Ok(())
}

fn main() {
    let _ = logging::init();
    std::panic::set_hook(Box::new(|info| {
        logging::error(format!("panic: {info}"));
    }));
    logging::info(format!(
        "application-start version={}",
        env!("CARGO_PKG_VERSION")
    ));
    if !privacy::acquire_single_instance() {
        logging::warn("application-start-rejected: another-instance-is-running");
        return;
    }
    let result = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_settings,
            persist_settings,
            reset_settings,
            pick_background,
            preview_background,
            privacy_status,
            start_privacy,
            minimize_window,
            close_app,
            open_log
        ])
        .run(tauri::generate_context!());
    match result {
        Ok(()) => logging::info("application-exit: ok"),
        Err(error) => logging::error(format!("application-exit: {error}")),
    }
}
