use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;

const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
const BACKUP_COUNT: usize = 2;

struct Logger {
    path: PathBuf,
}

static LOGGER: OnceLock<Mutex<Logger>> = OnceLock::new();

fn timestamp() -> String {
    let mut time: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut time) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond, time.wMilliseconds
    )
}

fn backup_path(path: &Path, index: usize) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(format!(".{index}"));
    PathBuf::from(value)
}

fn rotate_if_needed(path: &Path) -> std::io::Result<()> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(());
    };
    if metadata.len() < MAX_LOG_BYTES {
        return Ok(());
    }

    if BACKUP_COUNT > 0 {
        let oldest = backup_path(path, BACKUP_COUNT);
        if oldest.exists() {
            fs::remove_file(oldest)?;
        }
        for index in (1..BACKUP_COUNT).rev() {
            let source = backup_path(path, index);
            if source.exists() {
                fs::rename(source, backup_path(path, index + 1))?;
            }
        }
        fs::rename(path, backup_path(path, 1))?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn default_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("LocalPrivacyScreenRust")
        .join("logs")
        .join("privacy.log")
}

/// Initializes one process-wide log file. Logging failure never prevents the
/// privacy screen from starting.
pub fn init() -> Result<PathBuf, String> {
    if let Some(logger) = LOGGER.get() {
        return logger
            .lock()
            .map(|logger| logger.path.clone())
            .map_err(|error| format!("日志锁失效: {error}"));
    }

    let path = default_path();
    let parent = path
        .parent()
        .ok_or_else(|| "无法确定日志目录".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("创建日志目录失败: {error}"))?;
    rotate_if_needed(&path).map_err(|error| format!("轮转旧日志失败: {error}"))?;

    let logger = Logger { path: path.clone() };
    if LOGGER.set(Mutex::new(logger)).is_err() {
        return Err("日志系统已经初始化".into());
    }
    Ok(path)
}

pub fn path() -> Option<PathBuf> {
    LOGGER
        .get()
        .and_then(|logger| logger.lock().ok().map(|logger| logger.path.clone()))
}

fn write(level: &str, message: &str) {
    let Some(logger) = LOGGER.get() else {
        return;
    };
    let Ok(logger) = logger.lock() else {
        return;
    };
    let _ = rotate_if_needed(&logger.path);
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&logger.path)
    else {
        return;
    };
    let message = message.replace(['\r', '\n'], "\\n");
    let _ = writeln!(
        file,
        "{} [{}] [pid={} tid={}] {}",
        timestamp(),
        level,
        std::process::id(),
        unsafe { GetCurrentThreadId() },
        message
    );
    let _ = file.flush();
}

pub fn info(message: impl AsRef<str>) {
    write("INFO", message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    write("WARN", message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    write("ERROR", message.as_ref());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_logs_without_losing_current_file() {
        let root = std::env::temp_dir().join(format!("lps-log-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("privacy.log");
        fs::write(&path, vec![b'x'; MAX_LOG_BYTES as usize + 1]).unwrap();

        rotate_if_needed(&path).unwrap();

        assert!(!path.exists());
        assert!(backup_path(&path, 1).exists());
        let _ = fs::remove_dir_all(root);
    }
}
