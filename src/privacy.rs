//! Local privacy overlay: per-monitor topmost windows that are hidden from
//! screen capture, plus low-level hooks that drop physical keyboard/mouse
//! input while letting DuGJRemoteDesk and marked injected input through.

use crate::{logging, settings::Settings};
use image::{imageops, DynamicImage, Rgba, RgbaImage};
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM,
    LRESULT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, EnumDisplayMonitors, FillRect, GetMonitorInfoW, GetStockObject,
    StretchDIBits, UpdateWindow, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLACK_BRUSH,
    DIB_RGB_COLORS, HBRUSH,
    HDC, HMONITOR, MONITORINFO, PAINTSTRUCT, SRCCOPY,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Power::{
    SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL, VK_RMENU,
    VK_RSHIFT, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, GetMessageW, GetWindowLongPtrW, GetWindowRect, KillTimer, PostMessageW,
    PostQuitMessage, RegisterClassExW, SetLayeredWindowAttributes, SetTimer,
    SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos, SetWindowsHookExW, ShowWindow,
    TranslateMessage, UnhookWindowsHookEx, CREATESTRUCTW, GWLP_USERDATA, HC_ACTION, HTTRANSPARENT,
    HWND_TOPMOST, IsWindowVisible, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLMHF_INJECTED, LWA_ALPHA,
    MA_NOACTIVATE, MSG, MSLLHOOKSTRUCT, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    SW_SHOWNOACTIVATE, WDA_EXCLUDEFROMCAPTURE, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_APP, WM_CLOSE, WM_DISPLAYCHANGE, WM_ENDSESSION, WM_ERASEBKGND, WM_KEYDOWN,
    WM_KEYUP, WM_MOUSEACTIVATE, WM_NCCREATE, WM_NCHITTEST, WM_PAINT, WM_PRINTCLIENT,
    WM_QUERYENDSESSION, WM_SETCURSOR, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WNDCLASSEXW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

const WM_PRIVACY_LOCAL_EXIT: u32 = WM_APP + 0x381;
const WATCHDOG_TIMER: usize = 1;
const OVERLAY_CLASS: &str = "LocalPrivacyScreenRustOverlay";
const OWNER_CLASS: &str = "LocalPrivacyScreenRustOwner";

// Must match kRemoteInputMarker in remotely injected events. dwExtraInfo is a
// compatibility marker supplied by DuGJRemoteDesk, not a security boundary.
#[cfg(target_pointer_width = "64")]
const DGJ_REMOTE_INPUT_MARKER: usize = 0x4455_4445_534B_4D31; // "DUDESKM1"
#[cfg(target_pointer_width = "32")]
const DGJ_REMOTE_INPUT_MARKER: usize = 0x534B_4D31; // "SKM1"

pub const MOD_ALT: u32 = 1;
pub const MOD_CONTROL: u32 = 2;
pub const MOD_SHIFT: u32 = 4;
pub const MOD_WIN: u32 = 8;

static HOTKEY_MODIFIERS: AtomicU32 = AtomicU32::new(MOD_CONTROL | MOD_SHIFT);
static HOTKEY_VIRTUAL_KEY: AtomicU32 = AtomicU32::new(b'P' as u32);
static OWNER_WINDOW: AtomicIsize = AtomicIsize::new(0);
static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);
static PHYSICAL_KEYS: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];
static PHYSICAL_KEY_EVENTS: AtomicU32 = AtomicU32::new(0);
static INJECTED_KEY_EVENTS: AtomicU32 = AtomicU32::new(0);
static DGJ_KEY_EVENTS: AtomicU32 = AtomicU32::new(0);
static PHYSICAL_MOUSE_EVENTS: AtomicU32 = AtomicU32::new(0);
static INJECTED_MOUSE_EVENTS: AtomicU32 = AtomicU32::new(0);
static DGJ_MOUSE_EVENTS: AtomicU32 = AtomicU32::new(0);
static SINGLETON_HANDLE: AtomicIsize = AtomicIsize::new(0);
#[cfg(test)]
static OVERLAY_SNAPSHOT: std::sync::Mutex<Vec<isize>> = std::sync::Mutex::new(Vec::new());

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Returns false when another instance of the app is already running.
pub fn acquire_single_instance() -> bool {
    let name = wide("Local\\LocalPrivacyScreen.Rust.Standalone.v1");
    let handle = unsafe { CreateMutexW(null(), 1, name.as_ptr()) };
    if handle.is_null() {
        return false;
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe { CloseHandle(handle) };
        return false;
    }
    // Kept open for the whole process lifetime.
    SINGLETON_HANDLE.store(handle as isize, Ordering::SeqCst);
    true
}

/// Pre-scaled 32-bit top-down BGRX pixels for one monitor.
struct Bitmap {
    width: i32,
    height: i32,
    pixels: Vec<u8>,
}

struct OverlayContext {
    bitmap: Option<Bitmap>,
}

struct SessionState {
    image: Option<DynamicImage>,
    fit: String,
    overlays: Vec<HWND>,
    failed: bool,
}

fn monitor_rects() -> Vec<RECT> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _dc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let mut info: MONITORINFO = unsafe { zeroed() };
        info.cbSize = size_of::<MONITORINFO>() as u32;
        if unsafe { GetMonitorInfoW(monitor, &mut info) } != 0 {
            let monitors = unsafe { &mut *(data as *mut Vec<RECT>) };
            monitors.push(info.rcMonitor);
        }
        1
    }

    let mut monitors: Vec<RECT> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            null_mut(),
            null(),
            Some(collect),
            &mut monitors as *mut Vec<RECT> as LPARAM,
        );
    }
    monitors
}

pub(crate) fn fit_image(image: &DynamicImage, width: u32, height: u32, fit: &str) -> RgbaImage {
    use image::imageops::FilterType;
    match fit {
        "stretch" => image.resize_exact(width, height, FilterType::Triangle).to_rgba8(),
        "contain" => {
            let source_width = image.width().max(1) as f64;
            let source_height = image.height().max(1) as f64;
            let scale = f64::min(width as f64 / source_width, height as f64 / source_height);
            let draw_width = ((source_width * scale).round() as u32).clamp(1, width);
            let draw_height = ((source_height * scale).round() as u32).clamp(1, height);
            let resized = image.resize_exact(draw_width, draw_height, FilterType::Triangle);
            let mut canvas = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 255]));
            let x = (width - draw_width) / 2;
            let y = (height - draw_height) / 2;
            imageops::overlay(&mut canvas, &resized.to_rgba8(), x as i64, y as i64);
            canvas
        }
        _ => image.resize_to_fill(width, height, FilterType::Triangle).to_rgba8(),
    }
}

fn prepare_bitmap(image: &DynamicImage, width: u32, height: u32, fit: &str) -> Bitmap {
    let mut pixels = fit_image(image, width, height, fit).into_raw();
    // Composite over black, then convert RGBA -> BGRX for GDI.
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = pixel[3] as u32;
        let red = (pixel[0] as u32 * alpha / 255) as u8;
        let green = (pixel[1] as u32 * alpha / 255) as u8;
        let blue = (pixel[2] as u32 * alpha / 255) as u8;
        pixel.copy_from_slice(&[blue, green, red, 255]);
    }
    Bitmap {
        width: width as i32,
        height: height as i32,
        pixels,
    }
}

fn paint_overlay(window: HWND, dc: HDC, context: *const OverlayContext) {
    unsafe {
        let mut rect: RECT = zeroed();
        GetClientRect(window, &mut rect);
        let bitmap = if context.is_null() { None } else { (*context).bitmap.as_ref() };
        let Some(bitmap) = bitmap else {
            FillRect(dc, &rect, GetStockObject(BLACK_BRUSH) as HBRUSH);
            return;
        };
        let mut info: BITMAPINFO = zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: bitmap.width,
            biHeight: -bitmap.height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..zeroed()
        };
        StretchDIBits(
            dc,
            0,
            0,
            rect.right - rect.left,
            rect.bottom - rect.top,
            0,
            0,
            bitmap.width,
            bitmap.height,
            bitmap.pixels.as_ptr().cast(),
            &info,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    }
}

unsafe extern "system" fn overlay_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
            return DefWindowProcW(window, message, wparam, lparam);
        }
        let context = GetWindowLongPtrW(window, GWLP_USERDATA) as *const OverlayContext;
        match message {
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                let mut paint: PAINTSTRUCT = zeroed();
                let dc = BeginPaint(window, &mut paint);
                paint_overlay(window, dc, context);
                EndPaint(window, &paint);
                0
            }
            WM_PRINTCLIENT => {
                paint_overlay(window, wparam as HDC, context);
                0
            }
            WM_NCHITTEST => HTTRANSPARENT as LRESULT,
            WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
            WM_SETCURSOR => 1,
            _ => DefWindowProcW(window, message, wparam, lparam),
        }
    }
}

fn destroy_overlay(window: HWND) {
    unsafe {
        let context = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut OverlayContext;
        SetWindowLongPtrW(window, GWLP_USERDATA, 0);
        DestroyWindow(window);
        if !context.is_null() {
            drop(Box::from_raw(context));
        }
    }
}

impl SessionState {
    fn create_overlays(&mut self) -> Result<(), String> {
        self.destroy_overlays();
        let monitors = monitor_rects();
        if monitors.is_empty() {
            return Err("没有检测到显示器".into());
        }
        let class = wide(OVERLAY_CLASS);
        let title = wide("");
        for monitor in monitors {
            let width = (monitor.right - monitor.left).max(1);
            let height = (monitor.bottom - monitor.top).max(1);
            let bitmap = self
                .image
                .as_ref()
                .map(|image| prepare_bitmap(image, width as u32, height as u32, &self.fit));
            let context = Box::into_raw(Box::new(OverlayContext { bitmap }));
            let hwnd = unsafe {
                CreateWindowExW(
                    WS_EX_TOPMOST
                        | WS_EX_TOOLWINDOW
                        | WS_EX_TRANSPARENT
                        | WS_EX_NOACTIVATE
                        | WS_EX_LAYERED,
                    class.as_ptr(),
                    title.as_ptr(),
                    WS_POPUP,
                    monitor.left,
                    monitor.top,
                    width,
                    height,
                    null_mut(),
                    null_mut(),
                    GetModuleHandleW(null()),
                    context.cast(),
                )
            };
            if hwnd.is_null() {
                let code = unsafe { GetLastError() };
                unsafe { drop(Box::from_raw(context)) };
                return Err(format!("创建隐私窗口失败 (Windows={code})"));
            }
            self.overlays.push(hwnd);
            #[cfg(test)]
            OVERLAY_SNAPSHOT.lock().unwrap().push(hwnd as isize);
            unsafe {
                if SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA) == 0 {
                    return Err(format!(
                        "无法配置隐私窗口点击穿透 (Windows={})",
                        GetLastError()
                    ));
                }
                // Hidden from screen capture so the remote side still sees the desktop.
                if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) == 0 {
                    return Err(format!("无法排除屏幕捕获 (Windows={})", GetLastError()));
                }
                if SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    monitor.left,
                    monitor.top,
                    width,
                    height,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                ) == 0
                {
                    return Err(format!("显示隐私窗口失败 (Windows={})", GetLastError()));
                }
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                UpdateWindow(hwnd);
            }
        }
        if !self.covered() {
            return Err("隐私窗口未覆盖所有显示器".into());
        }
        Ok(())
    }

    fn destroy_overlays(&mut self) {
        for hwnd in self.overlays.drain(..) {
            destroy_overlay(hwnd);
        }
    }

    fn raise_overlays(&self) {
        for &hwnd in &self.overlays {
            unsafe {
                SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE);
            }
        }
    }

    fn covered(&self) -> bool {
        let monitors = monitor_rects();
        if monitors.is_empty() || self.overlays.len() != monitors.len() {
            return false;
        }
        monitors.iter().all(|monitor| {
            self.overlays.iter().any(|&hwnd| {
                let mut rect: RECT = unsafe { zeroed() };
                let ok = unsafe { IsWindowVisible(hwnd) != 0 && GetWindowRect(hwnd, &mut rect) != 0 };
                ok && rect.left <= monitor.left
                    && rect.top <= monitor.top
                    && rect.right >= monitor.right
                    && rect.bottom >= monitor.bottom
            })
        })
    }

    /// Called periodically and on display changes.
    fn ensure_coverage(&mut self) {
        if self.covered() {
            self.raise_overlays();
            return;
        }
        if let Err(error) = self.create_overlays() {
            logging::error(format!("display-coverage-failed: {error}"));
            self.failed = true;
            unsafe { PostQuitMessage(1) };
        }
    }
}

fn key_down(key: u16) -> bool {
    PHYSICAL_KEYS[key as usize].load(Ordering::Relaxed)
}

fn modifiers_match(modifiers: u32) -> bool {
    let control = key_down(VK_CONTROL) || key_down(VK_LCONTROL) || key_down(VK_RCONTROL);
    let alt = key_down(VK_MENU) || key_down(VK_LMENU) || key_down(VK_RMENU);
    let shift = key_down(VK_SHIFT) || key_down(VK_LSHIFT) || key_down(VK_RSHIFT);
    let win = key_down(VK_LWIN) || key_down(VK_RWIN);
    control == (modifiers & MOD_CONTROL != 0)
        && alt == (modifiers & MOD_ALT != 0)
        && shift == (modifiers & MOD_SHIFT != 0)
        && win == (modifiers & MOD_WIN != 0)
}

fn is_dgj_remote_input(extra_info: usize) -> bool {
    extra_info == DGJ_REMOTE_INPUT_MARKER
}

/// Tracks physical key state and requests exit when the configured hotkey
/// goes down. Returns true when an exit request was posted.
fn process_physical_key(vk: u32, down: bool) -> bool {
    if vk >= 256 {
        return false;
    }
    let was_down = PHYSICAL_KEYS[vk as usize].swap(down, Ordering::Relaxed);
    if !down
        || was_down
        || vk != HOTKEY_VIRTUAL_KEY.load(Ordering::Relaxed)
        || !modifiers_match(HOTKEY_MODIFIERS.load(Ordering::Relaxed))
        || EXIT_REQUESTED.load(Ordering::Relaxed)
    {
        return false;
    }
    let owner = OWNER_WINDOW.load(Ordering::Relaxed) as HWND;
    if owner.is_null() || unsafe { PostMessageW(owner, WM_PRIVACY_LOCAL_EXIT, 0, 0) } == 0 {
        return false;
    }
    EXIT_REQUESTED.store(true, Ordering::Relaxed);
    true
}

/// Drops local physical key events; injected and DuGJRemoteDesk input passes.
unsafe extern "system" fn keyboard_hook(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 {
            let event = &*(data as *const KBDLLHOOKSTRUCT);
            let dgj = is_dgj_remote_input(event.dwExtraInfo);
            if dgj {
                DGJ_KEY_EVENTS.fetch_add(1, Ordering::Relaxed);
            } else if event.flags & LLKHF_INJECTED != 0 {
                INJECTED_KEY_EVENTS.fetch_add(1, Ordering::Relaxed);
            } else {
                PHYSICAL_KEY_EVENTS.fetch_add(1, Ordering::Relaxed);
                let message = message as u32;
                if message == WM_KEYDOWN || message == WM_SYSKEYDOWN {
                    process_physical_key(event.vkCode, true);
                } else if message == WM_KEYUP || message == WM_SYSKEYUP {
                    process_physical_key(event.vkCode, false);
                }
                return 1;
            }
        }
        CallNextHookEx(null_mut(), code, message, data)
    }
}

unsafe extern "system" fn mouse_hook(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 {
            let event = &*(data as *const MSLLHOOKSTRUCT);
            let dgj = is_dgj_remote_input(event.dwExtraInfo);
            if dgj {
                DGJ_MOUSE_EVENTS.fetch_add(1, Ordering::Relaxed);
            } else if event.flags & LLMHF_INJECTED != 0 {
                INJECTED_MOUSE_EVENTS.fetch_add(1, Ordering::Relaxed);
            } else {
                PHYSICAL_MOUSE_EVENTS.fetch_add(1, Ordering::Relaxed);
                return 1;
            }
        }
        CallNextHookEx(null_mut(), code, message, data)
    }
}

unsafe extern "system" fn owner_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
            return DefWindowProcW(window, message, wparam, lparam);
        }
        // Borrowed pointer; the session is owned by run_privacy.
        let state = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut SessionState;
        if state.is_null() {
            return DefWindowProcW(window, message, wparam, lparam);
        }
        match message {
            WM_PRIVACY_LOCAL_EXIT => {
                logging::info("privacy-exit: local-hotkey");
                PostQuitMessage(0);
                0
            }
            WM_CLOSE => {
                logging::info("privacy-exit: owner-close");
                PostQuitMessage(0);
                0
            }
            WM_QUERYENDSESSION => 1,
            WM_ENDSESSION => {
                if wparam != 0 {
                    logging::info("privacy-exit: windows-session-end");
                    PostQuitMessage(0);
                }
                0
            }
            WM_DISPLAYCHANGE => {
                logging::info("display-change");
                if let Err(error) = (*state).create_overlays() {
                    logging::error(format!("display-change-failed: {error}"));
                    (*state).failed = true;
                    PostQuitMessage(1);
                }
                0
            }
            WM_TIMER if wparam == WATCHDOG_TIMER => {
                (*state).ensure_coverage();
                0
            }
            _ => DefWindowProcW(window, message, wparam, lparam),
        }
    }
}

fn register_class(name: &[u16], proc_: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT) -> Result<(), String> {
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(proc_),
        hInstance: unsafe { GetModuleHandleW(null()) },
        lpszClassName: name.as_ptr(),
        ..unsafe { zeroed() }
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        let code = unsafe { GetLastError() };
        if code != ERROR_CLASS_ALREADY_EXISTS {
            return Err(format!("注册窗口类失败 (Windows={code})"));
        }
    }
    Ok(())
}

fn reset_input_event_counts() {
    PHYSICAL_KEY_EVENTS.store(0, Ordering::Relaxed);
    INJECTED_KEY_EVENTS.store(0, Ordering::Relaxed);
    DGJ_KEY_EVENTS.store(0, Ordering::Relaxed);
    PHYSICAL_MOUSE_EVENTS.store(0, Ordering::Relaxed);
    INJECTED_MOUSE_EVENTS.store(0, Ordering::Relaxed);
    DGJ_MOUSE_EVENTS.store(0, Ordering::Relaxed);
}

/// Runs the privacy screen on the calling thread until the exit hotkey is
/// pressed on the local keyboard. Must not be called on the UI thread.
pub fn run_privacy(settings: &Settings) -> Result<(), String> {
    let started = std::time::Instant::now();
    reset_input_event_counts();
    logging::info(format!(
        "privacy-session-begin hotkey={} fit={} background={}",
        settings.hotkey.label,
        settings.fit,
        if settings.background_path.trim().is_empty() {
            "none"
        } else {
            "custom"
        }
    ));

    let result = run_privacy_inner(settings);
    logging::info(format!(
        "privacy-session-end status={} duration_ms={} physical_keyboard={} injected_keyboard={} dugj_keyboard={} physical_mouse={} injected_mouse={} dugj_mouse={}",
        if result.is_ok() { "ok" } else { "error" },
        started.elapsed().as_millis(),
        PHYSICAL_KEY_EVENTS.load(Ordering::Relaxed),
        INJECTED_KEY_EVENTS.load(Ordering::Relaxed),
        DGJ_KEY_EVENTS.load(Ordering::Relaxed),
        PHYSICAL_MOUSE_EVENTS.load(Ordering::Relaxed),
        INJECTED_MOUSE_EVENTS.load(Ordering::Relaxed),
        DGJ_MOUSE_EVENTS.load(Ordering::Relaxed),
    ));
    result
}

fn run_privacy_inner(settings: &Settings) -> Result<(), String> {
    let image = if settings.background_path.trim().is_empty() {
        None
    } else {
        Some(
            image::open(&settings.background_path)
                .map_err(|error| format!("读取背景图片失败: {error}"))?,
        )
    };

    let overlay_class = wide(OVERLAY_CLASS);
    let owner_class = wide(OWNER_CLASS);
    register_class(&overlay_class, overlay_proc)?;
    register_class(&owner_class, owner_proc)?;

    let mut state = Box::new(SessionState {
        image,
        fit: settings.fit.clone(),
        overlays: Vec::new(),
        failed: false,
    });
    let state_ptr: *mut SessionState = &mut *state;
    let title = wide("LocalPrivacyScreenRust");
    let owner = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            owner_class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            state_ptr.cast(),
        )
    };
    if owner.is_null() {
        return Err(format!("创建控制窗口失败 (Windows={})", unsafe { GetLastError() }));
    }

    HOTKEY_MODIFIERS.store(settings.hotkey.modifiers, Ordering::SeqCst);
    HOTKEY_VIRTUAL_KEY.store(settings.hotkey.virtual_key, Ordering::SeqCst);
    EXIT_REQUESTED.store(false, Ordering::SeqCst);
    for key in &PHYSICAL_KEYS {
        key.store(false, Ordering::Relaxed);
    }
    OWNER_WINDOW.store(owner as isize, Ordering::SeqCst);

    let result = unsafe { run_session(owner, state_ptr) };

    OWNER_WINDOW.store(0, Ordering::SeqCst);
    EXIT_REQUESTED.store(false, Ordering::SeqCst);
    for key in &PHYSICAL_KEYS {
        key.store(false, Ordering::Relaxed);
    }
    unsafe {
        (*state_ptr).destroy_overlays();
        SetWindowLongPtrW(owner, GWLP_USERDATA, 0);
        DestroyWindow(owner);
    }
    let failed = state.failed;
    drop(state);
    result?;
    if failed {
        return Err("隐私屏意外停止".into());
    }
    Ok(())
}

unsafe fn run_session(owner: HWND, state: *mut SessionState) -> Result<(), String> {
    unsafe {
        (*state).create_overlays()?;
        let instance = GetModuleHandleW(null());
        let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), instance, 0);
        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), instance, 0);
        let timer = SetTimer(owner, WATCHDOG_TIMER, 500, None);
        let mut execution_state_claimed = false;
        let result = if keyboard.is_null() || mouse.is_null() {
            Err(format!("安装键鼠拦截失败 (Windows={})", GetLastError()))
        } else if timer == 0 {
            Err("启动隐私屏监控失败".into())
        } else {
            execution_state_claimed =
                SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED)
                    != 0;
            if !execution_state_claimed {
                Err("无法阻止系统休眠".into())
            } else {
                logging::info(format!(
                    "privacy-session-ready overlays={} fit={}",
                    (*state).overlays.len(),
                    (*state).fit
                ));
                let mut message: MSG = zeroed();
                loop {
                    let get_message_result = GetMessageW(&mut message, null_mut(), 0, 0);
                    if get_message_result == -1 {
                        break Err(format!("隐私屏消息循环失败 (Windows={})", GetLastError()));
                    }
                    if get_message_result == 0 {
                        break Ok(());
                    }
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        };
        if execution_state_claimed {
            SetThreadExecutionState(ES_CONTINUOUS);
        }
        if timer != 0 {
            KillTimer(owner, WATCHDOG_TIMER);
        }
        if !keyboard.is_null() {
            UnhookWindowsHookEx(keyboard);
        }
        if !mouse.is_null() {
            UnhookWindowsHookEx(mouse);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::GENERIC_ALL;
    use windows_sys::Win32::System::StationsAndDesktops::{
        CloseDesktop, CreateDesktopW, SetThreadDesktop,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowDisplayAffinity, IsWindow, GWL_EXSTYLE,
    };

    // Hook state is process-global, so tests touching it run one at a time.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn red_image(width: u32, height: u32) -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(width, height, Rgba([255, 0, 0, 255])))
    }

    #[test]
    fn contain_letterboxes_with_black() {
        let fitted = fit_image(&red_image(100, 100), 200, 100, "contain");
        assert_eq!(fitted.dimensions(), (200, 100));
        assert_eq!(fitted.get_pixel(5, 50).0, [0, 0, 0, 255]);
        assert_eq!(fitted.get_pixel(100, 50).0, [255, 0, 0, 255]);
    }

    #[test]
    fn cover_and_stretch_fill_the_monitor() {
        for fit in ["cover", "stretch"] {
            let fitted = fit_image(&red_image(100, 100), 200, 100, fit);
            assert_eq!(fitted.dimensions(), (200, 100));
            assert_eq!(fitted.get_pixel(1, 1).0, [255, 0, 0, 255], "{fit}");
            assert_eq!(fitted.get_pixel(198, 98).0, [255, 0, 0, 255], "{fit}");
        }
    }

    #[test]
    fn bitmap_is_bgrx_composited_over_black() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 2, Rgba([255, 0, 0, 128])));
        let bitmap = prepare_bitmap(&image, 2, 2, "stretch");
        assert_eq!(&bitmap.pixels[0..4], &[0, 0, 128, 255]);
    }

    fn key_event(vk: u16, injected: bool) -> KBDLLHOOKSTRUCT {
        KBDLLHOOKSTRUCT {
            vkCode: vk as u32,
            scanCode: 0,
            flags: if injected { LLKHF_INJECTED } else { 0 },
            time: 0,
            dwExtraInfo: 0,
        }
    }

    fn key_event_with_extra_info(
        vk: u16,
        injected: bool,
        extra_info: usize,
    ) -> KBDLLHOOKSTRUCT {
        KBDLLHOOKSTRUCT {
            dwExtraInfo: extra_info,
            ..key_event(vk, injected)
        }
    }

    fn send_key(vk: u16, down: bool, injected: bool) -> LRESULT {
        let event = key_event(vk, injected);
        let message = if down { WM_KEYDOWN } else { WM_KEYUP };
        unsafe { keyboard_hook(HC_ACTION as i32, message as WPARAM, &event as *const _ as LPARAM) }
    }

    fn send_dgj_remote_key(vk: u16, down: bool) -> LRESULT {
        let event = key_event_with_extra_info(vk, false, DGJ_REMOTE_INPUT_MARKER);
        let message = if down { WM_KEYDOWN } else { WM_KEYUP };
        unsafe { keyboard_hook(HC_ACTION as i32, message as WPARAM, &event as *const _ as LPARAM) }
    }

    /// Runs on the isolated desktop: verifies overlays, then drives the hook.
    fn drive_session() -> Result<Vec<isize>, String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while OWNER_WINDOW.load(Ordering::SeqCst) == 0 || OVERLAY_SNAPSHOT.lock().unwrap().is_empty() {
            if Instant::now() > deadline {
                return Err("privacy session did not start".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(300));
        let overlays = OVERLAY_SNAPSHOT.lock().unwrap().clone();
        for &overlay in &overlays {
            let hwnd = overlay as HWND;
            let mut affinity = 0u32;
            unsafe {
                if IsWindow(hwnd) == 0 {
                    return Err("overlay window is missing".into());
                }
                if GetWindowDisplayAffinity(hwnd, &mut affinity) == 0 || affinity != WDA_EXCLUDEFROMCAPTURE {
                    return Err(format!("overlay is not excluded from capture: {affinity:#x}"));
                }
                if GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST == 0 {
                    return Err("overlay is not topmost".into());
                }
                if GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_LAYERED == 0 {
                    return Err("overlay is not layered".into());
                }
            }
        }

    fn send_mouse(extra_info: usize) -> LRESULT {
        let event = MSLLHOOKSTRUCT {
            pt: Default::default(),
            mouseData: 0,
            flags: 0,
            time: 0,
            dwExtraInfo: extra_info,
        };
        unsafe {
            mouse_hook(
                HC_ACTION as i32,
                windows_sys::Win32::UI::WindowsAndMessaging::WM_MOUSEMOVE as WPARAM,
                &event as *const _ as LPARAM,
            )
        }
    }

        // Remote (injected) input passes through, even the hotkey itself.
        for vk in [VK_LCONTROL, VK_LSHIFT, b'P' as u16] {
            if send_key(vk, true, true) == 1 {
                return Err("injected key was blocked".into());
            }
        }
        if EXIT_REQUESTED.load(Ordering::SeqCst) {
            return Err("injected hotkey stopped privacy mode".into());
        }

        // The in-house remote desktop marker is accepted even if a particular
        // Windows/IPC path did not preserve the injected flag.
        for vk in [VK_LCONTROL, VK_LSHIFT, b'P' as u16] {
            if send_dgj_remote_key(vk, true) == 1 {
                return Err("DuGJRemoteDesk key was blocked".into());
            }
        }
        for vk in [b'P' as u16, VK_LSHIFT, VK_LCONTROL] {
            if send_dgj_remote_key(vk, false) == 1 {
                return Err("DuGJRemoteDesk key-up was blocked".into());
            }
        }
        if EXIT_REQUESTED.load(Ordering::SeqCst) {
            return Err("DuGJRemoteDesk hotkey stopped privacy mode".into());
        }

        if send_mouse(0) != 1 {
            return Err("physical mouse was not blocked".into());
        }
        if send_mouse(DGJ_REMOTE_INPUT_MARKER) == 1 {
            return Err("DuGJRemoteDesk mouse was blocked".into());
        }

        // Physical input is blocked; an incomplete combination does not exit.
        for (vk, down) in [(VK_LCONTROL, true), (b'P' as u16, true), (b'P' as u16, false), (VK_LCONTROL, false)] {
            if send_key(vk, down, false) != 1 {
                return Err("physical key was not blocked".into());
            }
        }
        if EXIT_REQUESTED.load(Ordering::SeqCst) {
            return Err("wrong physical hotkey stopped privacy mode".into());
        }

        send_key(VK_LCONTROL, true, false);
        send_key(VK_LSHIFT, true, false);
        send_key(b'P' as u16, true, false);
        if !EXIT_REQUESTED.load(Ordering::SeqCst) {
            return Err("configured physical hotkey was ignored".into());
        }
        Ok(overlays)
    }

    #[test]
    fn lifecycle_on_isolated_desktop() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        OVERLAY_SNAPSHOT.lock().unwrap().clear();

        let image_path = std::env::temp_dir().join(format!("lps-rust-test-{}.png", std::process::id()));
        red_image(64, 36).save(&image_path).expect("write test image");
        let settings = Settings {
            background_path: image_path.to_string_lossy().to_string(),
            ..Settings::default()
        };

        let name = wide(&format!("LocalPrivacyRustTest-{}", std::process::id()));
        let desktop = unsafe { CreateDesktopW(name.as_ptr(), null(), null(), 0, GENERIC_ALL, null()) };
        assert!(!desktop.is_null(), "cannot create test desktop");
        let desktop_handle = desktop as isize;

        let driver = std::thread::spawn(move || {
            unsafe { SetThreadDesktop(desktop_handle as _) };
            drive_session()
        });
        let session = std::thread::spawn(move || {
            if unsafe { SetThreadDesktop(desktop_handle as _) } == 0 {
                return Err("cannot switch to test desktop".to_string());
            }
            run_privacy(&settings)
        });

        let session_result = session.join().expect("session thread panicked");
        let driver_result = driver.join().expect("driver thread panicked");
        let _ = std::fs::remove_file(&image_path);
        unsafe { CloseDesktop(desktop) };

        session_result.expect("privacy session failed");
        let overlays = driver_result.expect("session checks failed");
        assert!(!overlays.is_empty());
        for overlay in overlays {
            assert_eq!(unsafe { IsWindow(overlay as HWND) }, 0, "overlay was not destroyed");
        }
        assert_eq!(OWNER_WINDOW.load(Ordering::SeqCst), 0);
        assert!(!EXIT_REQUESTED.load(Ordering::SeqCst));
    }
}
