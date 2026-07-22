mod db;
mod llm;
mod models;
mod ocr;
mod speech;

#[cfg(windows)]
mod keyboard_windows;
#[cfg(windows)]
mod recording_windows;

use crate::db::Database;
use crate::models::*;
use anyhow::{Context, Result};
use parking_lot::Mutex;
use std::collections::HashMap;
use tauri_plugin_clipboard_manager::ClipboardExt;
#[cfg(target_os = "macos")]
use std::io::{BufRead, BufReader, Write};
#[cfg(target_os = "macos")]
use std::sync::mpsc;
use std::process::Child;
#[cfg(target_os = "macos")]
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::Duration;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, PhysicalSize, Position, RunEvent, Size,
    WindowEvent,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as AutostartExt};

#[cfg(target_os = "macos")]
extern "C" {
    fn redkey_microphone_authorization_status() -> std::os::raw::c_int;
    fn redkey_request_microphone_permission();
}

struct RuntimeState {
    db: Mutex<Option<Database>>,
    tray_icon: Mutex<Option<TrayIcon>>,
    keyboard_monitor: Mutex<Option<KeyboardMonitor>>,
    hover_regions: Mutex<HoverRegions>,
    speech_worker: Mutex<Option<speech::SpeechWorker>>,
    processing_cancellations: Mutex<HashMap<String, Arc<AtomicBool>>>,
    native_recording: Mutex<Option<NativeRecording>>,
    hud_state: Mutex<HudState>,
    pet_mode: Mutex<String>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            db: Mutex::new(None),
            tray_icon: Mutex::new(None),
            keyboard_monitor: Mutex::new(None),
            hover_regions: Mutex::new(HoverRegions::default()),
            speech_worker: Mutex::new(None),
            processing_cancellations: Mutex::new(HashMap::new()),
            native_recording: Mutex::new(None),
            hud_state: Mutex::new(HudState::default()),
            pet_mode: Mutex::new("default".to_string()),
        }
    }
}

impl RuntimeState {
    fn db(&self) -> parking_lot::MappedMutexGuard<'_, Database> {
        let mut guard = self.db.lock();
        if guard.is_none() {
            eprintln!("Database not initialized, falling back to in-memory database");
            if let Ok(memory_db) = Database::memory() {
                *guard = Some(memory_db);
            }
        }
        parking_lot::MutexGuard::map(guard, |opt| {
            opt.as_mut().expect("Database not initialized")
        })
    }
}

#[cfg(target_os = "macos")]
struct NativeRecording { id: String, path: std::path::PathBuf, started: std::time::Instant, child: Child, input: ChildStdin }
#[cfg(windows)]
type NativeRecording = recording_windows::NativeRecording;
#[cfg(target_os = "macos")]
struct KeyboardMonitor {
    config: std::sync::Arc<Mutex<PrefixConfig>>,
    error: std::sync::Arc<Mutex<Option<String>>>,
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    sender: mpsc::Sender<KeyboardEvent>,
}

#[cfg(windows)]
type KeyboardMonitor = keyboard_windows::KeyboardMonitor;

#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
struct PrefixConfig { required: core_graphics::event::CGEventFlags }

#[cfg(target_os = "macos")]
enum KeyboardEvent { Prefix(bool), Action(AppAction) }

#[derive(Default)]
struct HudState { prefix_held: bool }

#[derive(Default)]
struct HoverRegions {
    pet: bool,
    panel: bool,
    dragging: bool,
    generation: u64,
}


fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// 在 Windows 上为子进程设置 CREATE_NO_WINDOW 标志，避免弹出控制台窗口；
/// 其他平台为 no-op。供 llm/speech 等模块共享。
#[cfg(windows)]
pub(crate) fn no_window(cmd: &mut std::process::Command) -> &mut std::process::Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x08000000)
}
#[cfg(not(windows))]
pub(crate) fn no_window(cmd: &mut std::process::Command) -> &mut std::process::Command { cmd }

fn snapshot(app: &AppHandle) -> Result<Snapshot> {
    app.state::<RuntimeState>().db().snapshot()
}

fn emit_snapshot(app: &AppHandle) -> Result<Snapshot> {
    let value = snapshot(app)?;
    app.emit("redkey://snapshot", &value)?;
    Ok(value)
}

fn transcribe_with_worker(app: &AppHandle, path: &std::path::Path, request_id: &str) -> Result<Vec<crate::models::SpeakerSegment>> {
    let state = app.state::<RuntimeState>();
    let mut guard = state.speech_worker.lock();
    if guard.is_none() {
        let worker = speech::SpeechWorker::start(app)?;
        *guard = Some(worker);
    }
    if processing_cancelled(app, request_id) {
        *guard = None;
        return Err(anyhow::anyhow!("录音处理已取消"));
    }
    match guard.as_mut().unwrap().transcribe(path) {
        Ok(segments) => Ok(segments),
        Err(error) => { *guard = None; Err(error) }
    }
}

fn begin_processing(app: &AppHandle, recording_id: &str) -> Arc<AtomicBool> {
    let token = Arc::new(AtomicBool::new(false));
    app.state::<RuntimeState>().processing_cancellations.lock().insert(recording_id.into(), token.clone());
    token
}

fn processing_cancelled(app: &AppHandle, recording_id: &str) -> bool {
    app.state::<RuntimeState>().processing_cancellations.lock().get(recording_id).is_some_and(|token| token.load(Ordering::Acquire))
}

fn finish_processing(app: &AppHandle, recording_id: &str) {
    app.state::<RuntimeState>().processing_cancellations.lock().remove(recording_id);
}

fn cancel_processing(app: &AppHandle, recording_id: &str) {
    if let Some(token) = app.state::<RuntimeState>().processing_cancellations.lock().get(recording_id) {
        token.store(true, Ordering::Release);
    }
    app.state::<RuntimeState>().speech_worker.lock().take();
}

fn run_transcription_pipeline(app: AppHandle, path: std::path::PathBuf, recording_id: String) {
    if processing_cancelled(&app, &recording_id) { finish_processing(&app, &recording_id); return; }
    app.state::<RuntimeState>().db().set_processing_status(&recording_id, "transcribing", None).ok();
    let _ = emit_snapshot(&app);
    let result = transcribe_with_worker(&app, &path, &recording_id);
    if processing_cancelled(&app, &recording_id) { finish_processing(&app, &recording_id); return; }
    match result {
        Ok(segments) => {
            let text = segments.iter().map(|s| format!("{}: {}", s.speaker, s.text)).collect::<Vec<_>>().join("\n");
            let raw = segments.iter().map(|s| s.text.clone()).collect::<Vec<_>>().join("\n");
            if app.state::<RuntimeState>().db().complete_transcription(&recording_id, &raw, &text, &segments).is_ok() && !processing_cancelled(&app, &recording_id) {
                spawn_recording_summary(&app, &recording_id);
            }
        }
        Err(error) => {
            if !processing_cancelled(&app, &recording_id) { let _ = app.state::<RuntimeState>().db().fail_recording(&recording_id, &error.to_string()); }
        }
    }
    finish_processing(&app, &recording_id);
    let _ = emit_snapshot(&app);
}

fn spawn_recording_summary(app: &AppHandle, recording_id: &str) {
    spawn_recording_summary_with_force(app, recording_id, false);
}

fn spawn_recording_summary_with_force(app: &AppHandle, recording_id: &str, force: bool) {
    let app = app.clone();
    let recording_id = recording_id.to_string();
    tauri::async_runtime::spawn(async move {
        if !llm::settings().map(|settings| settings.configured).unwrap_or(false) { return; }
        let document = match app.state::<RuntimeState>().db().task_document_for_recording(&recording_id) {
            Ok(document) => document,
            Err(error) => { let _ = app.state::<RuntimeState>().db().set_recording_summary_status(&recording_id, "error", Some(&error.to_string())); let _ = emit_snapshot(&app); return; }
        };
        if !force && document.summaries.iter().any(|summary| summary.recording_id == recording_id && summary.user_edited) {
            let _ = emit_snapshot(&app);
            return;
        }
        let _ = app.state::<RuntimeState>().db().set_recording_summary_status(&recording_id, "summarizing", None);
        let _ = emit_snapshot(&app);
        match llm::summarize(&document, &recording_id).await {
            Ok(summary) => { let _ = app.state::<RuntimeState>().db().save_recording_summary(&summary); }
            Err(error) => { let _ = app.state::<RuntimeState>().db().set_recording_summary_status(&recording_id, "error", Some(&error.to_string())); }
        }
        let _ = emit_snapshot(&app);
    });
}

fn show_and_focus_window(app: &AppHandle, label: &str) -> Result<tauri::WebviewWindow> {
    let window = app
        .get_webview_window(label)
        .context(format!("{label} 窗口不存在"))?;
    window.show()?;
    window.unminimize()?;
    window.set_focus()?;
    Ok(window)
}

fn show_console_window(app: &AppHandle) -> Result<()> {
    show_and_focus_window(app, "console")?;
    Ok(())
}

fn show_settings_window(app: &AppHandle) -> Result<()> {
    let window = show_and_focus_window(app, "console")?;
    let mut url = window.url()?;
    url.set_query(Some("view=settings"));
    window.navigate(url)?;
    Ok(())
}

fn open_link(url: &str) -> Result<()> {
    open::that(url).context("无法使用默认浏览器打开链接")?;
    Ok(())
}

pub fn dispatch_internal(app: &AppHandle, action: AppAction) -> Result<Snapshot> {
    if action == AppAction::OpenConsole {
        show_console_window(app)?;
        return emit_snapshot(app);
    }
    if action == AppAction::ToggleRecording {
        app.emit("redkey://recording-toggle", ())?;
        return snapshot(app);
    }
    let url = {
        let state = app.state::<RuntimeState>();
        let mut db = state.db();
        db.dispatch(&action)?
    };
    if let Some(url) = url {
        open_link(&url)?;
    }
    emit_snapshot(app)
}

const HUD_HEIGHT: u32 = 124;

fn position_hud(app: &AppHandle) -> Result<tauri::WebviewWindow> {
    let hud = app.get_webview_window("hud").context("提示窗口不存在")?;
    let monitor = hud.primary_monitor()?.context("无法读取主显示器")?;
    let work_area = monitor.work_area();
    let width = work_area.size.width as u32;
    let desired = Size::Physical(PhysicalSize::new(width, HUD_HEIGHT));
    hud.set_size(desired)?;
    let x = work_area.position.x;
    let y = work_area.position.y + work_area.size.height as i32 - HUD_HEIGHT as i32;
    hud.set_position(Position::Physical(PhysicalPosition::new(x, y)))?;
    hud.set_always_on_top(true)?;
    #[cfg(target_os = "macos")]
    hud.set_visible_on_all_workspaces(true)?;
    // HUD 现在需要接收鼠标悬停/点击事件，但保持 focusable=false 不抢键盘焦点。
    hud.set_ignore_cursor_events(false)?;
    Ok(hud)
}

pub fn emit_task_hud(app: &AppHandle) -> Result<()> {
    let snapshot = snapshot(app)?;
    let slots = (0..10).map(|slot| {
        let task = snapshot.tasks.iter().find(|task| task.group == snapshot.current_group && task.slot == Some(slot) && task.status == "active");
        serde_json::json!({
            "slot": slot,
            "task_id": task.map(|task| task.id.clone()),
            "name": task.and_then(|task| task.contact_name.clone()),
            "title": task.map(|task| task.source_title.clone().unwrap_or_else(|| task.title.clone()))
        })
    }).collect::<Vec<_>>();
    let hud = position_hud(app)?;
    hud.show()?;
    hud.emit("redkey://task-hud", serde_json::json!({ "slots": slots }))?;
    Ok(())
}

pub fn set_task_hud_visible(app: &AppHandle, visible: bool) -> Result<()> {
    let runtime = app.state::<RuntimeState>();
    let mut state = runtime.hud_state.lock();
    state.prefix_held = visible;
    drop(state);
    if visible {
        emit_task_hud(app)?;
    } else {
        if let Some(hud) = app.get_webview_window("hud") {
            let _ = hud.emit("redkey://task-hud", serde_json::json!({ "slots": [] }));
            let _ = hud.set_ignore_cursor_events(true);
            hud.hide()?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn prefix_config(value: &str) -> PrefixConfig {
    use core_graphics::event::CGEventFlags;
    let mut required = CGEventFlags::default();
    for item in value.split('+').map(str::trim) {
        required |= match item { "Control" => CGEventFlags::CGEventFlagControl, "Alt" | "Option" => CGEventFlags::CGEventFlagAlternate, "Shift" => CGEventFlags::CGEventFlagShift, "Command" => CGEventFlags::CGEventFlagCommand, _ => CGEventFlags::default() };
    }
    PrefixConfig { required }
}

#[cfg(target_os = "macos")]
fn keyboard_action(key_code: i64) -> Option<AppAction> {
    Some(match key_code {
        18 => AppAction::ActivateSlot { slot: 0 }, 19 => AppAction::ActivateSlot { slot: 1 }, 20 => AppAction::ActivateSlot { slot: 2 },
        21 => AppAction::ActivateSlot { slot: 3 }, 23 => AppAction::ActivateSlot { slot: 4 }, 22 => AppAction::ActivateSlot { slot: 5 },
        26 => AppAction::ActivateSlot { slot: 6 }, 28 => AppAction::ActivateSlot { slot: 7 }, 25 => AppAction::ActivateSlot { slot: 8 },
        29 => AppAction::ActivateSlot { slot: 9 },
        17 => AppAction::ToggleRecording,
        _ => return None,
    })
}

#[cfg(target_os = "macos")]
fn install_keyboard_tap(app: &AppHandle, monitor: &KeyboardMonitor) {
    use core_graphics::event::{CallbackResult, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType, EventField};
    use core_foundation::runloop::CFRunLoop;
    if monitor.running.swap(true, std::sync::atomic::Ordering::SeqCst) { return; }
    let config = monitor.config.clone();
    let error = monitor.error.clone();
    let running = monitor.running.clone();
    let sender = monitor.sender.clone();
    let _app = app.clone();
    std::thread::spawn(move || {
        let result = CGEventTap::with_enabled(CGEventTapLocation::Session, CGEventTapPlacement::HeadInsertEventTap, CGEventTapOptions::Default, vec![CGEventType::KeyDown, CGEventType::KeyUp, CGEventType::FlagsChanged], {
            let prefix_active = std::sync::atomic::AtomicBool::new(false);
            move |_proxy, event_type, event| {
                let settings = *config.lock();
                let flags = event.get_flags();
                let modifier_flags = CGEventFlags::CGEventFlagShift | CGEventFlags::CGEventFlagControl | CGEventFlags::CGEventFlagAlternate | CGEventFlags::CGEventFlagCommand;
                let required = settings.required;
                let has_required = !required.is_empty() && flags.intersection(required) == required;
                let clean = has_required && flags.intersection(modifier_flags) == required;
                if clean != prefix_active.swap(clean, std::sync::atomic::Ordering::SeqCst) { let _ = sender.send(KeyboardEvent::Prefix(clean)); }
                if !matches!(event_type, CGEventType::KeyDown) || !has_required { return CallbackResult::Keep; }
                let Some(action) = keyboard_action(event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE)) else { return CallbackResult::Keep; };
                let extras = flags.intersection(modifier_flags).difference(required);
                if extras.is_empty() { let _ = sender.send(KeyboardEvent::Action(action)); return CallbackResult::Drop; }
                CallbackResult::Keep
            }
        }, CFRunLoop::run_current);
        if result.is_err() { *error.lock() = Some("missing accessibility permission".into()); }
        running.store(false, std::sync::atomic::Ordering::SeqCst);
    });
}

#[cfg(target_os = "macos")]
fn start_keyboard_monitor(app: &AppHandle, settings: &ShortcutSettings) -> KeyboardMonitor {
    let config = std::sync::Arc::new(Mutex::new(prefix_config(&settings.task_prefix)));
    let error = std::sync::Arc::new(Mutex::new(None));
    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel();
    let app_handle = app.clone();
    std::thread::spawn(move || for event in receiver {
        match event {
            KeyboardEvent::Prefix(active) => { let _ = set_task_hud_visible(&app_handle, active); }
            KeyboardEvent::Action(AppAction::ActivateSlot { slot }) => { let _ = dispatch_internal(&app_handle, AppAction::ActivateSlot { slot }); }
            KeyboardEvent::Action(action) => { let _ = dispatch_internal(&app_handle, action); }
        }
    });
    let monitor = KeyboardMonitor { config, error, running, sender };
    install_keyboard_tap(app, &monitor);
    monitor
}

#[cfg(windows)]
fn start_keyboard_monitor(app: &AppHandle, settings: &ShortcutSettings) -> KeyboardMonitor {
    keyboard_windows::start_keyboard_monitor(app, settings)
}

fn update_keyboard_listener(app: &AppHandle, settings: &ShortcutSettings) -> Result<()> {
    settings.validate()?;
    let state = app.state::<RuntimeState>();
    let mut monitor = state.keyboard_monitor.lock();
    if let Some(monitor) = monitor.as_ref() {
        #[cfg(target_os = "macos")]
        {
            *monitor.config.lock() = prefix_config(&settings.task_prefix);
            *monitor.error.lock() = None;
            install_keyboard_tap(app, monitor);
        }
        #[cfg(windows)]
        {
            monitor.update_config(keyboard_windows::PrefixConfig::from_string(&settings.task_prefix));
        }
    } else {
        *monitor = Some(start_keyboard_monitor(app, settings));
    }
    Ok(())
}

fn set_pet_visible_inner(app: &AppHandle, visible: bool) -> Result<()> {
    let pet = app.get_webview_window("pet").context("宠物窗口不存在")?;
    if visible { pet.show()?; } else { pet.hide()?; }
    let mut settings = app.state::<RuntimeState>().db().settings()?;
    settings.pet_visible = visible;
    app.state::<RuntimeState>().db().save_settings(&settings)?;
    Ok(())
}

fn setup_tray(app: &tauri::App) -> Result<()> {
    let show = MenuItem::with_id(app, "show", "打开控制台", true, None::<&str>)?;
    let pet_visible = app.state::<RuntimeState>().db().settings()?.pet_visible;
    let toggle_pet = MenuItem::with_id(app, "toggle_pet", if pet_visible { "休眠宠物" } else { "唤醒宠物" }, true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &toggle_pet, &settings, &quit])?;
    let icon = app
        .default_window_icon()
        .context("应用图标不存在，无法创建菜单栏图标")?
        .clone();
    let builder = TrayIconBuilder::with_id("redkey-tray")
        .menu(&menu)
        .tooltip("AlphaKey")
        .icon(icon)
        .icon_as_template(true)
        .show_menu_on_left_click(true);
    let tray = builder
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "show" => {
                let _ = show_console_window(app);
            }
            "settings" => {
                let _ = show_settings_window(app);
            }
            "toggle_pet" => {
                if let Some(pet) = app.get_webview_window("pet") {
                    let visible = !pet.is_visible().unwrap_or(false);
                    if set_pet_visible_inner(app, visible).is_ok() {
                        let _ = toggle_pet.set_text(if visible { "休眠宠物" } else { "唤醒宠物" });
                    }
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)
        .context("无法创建系统托盘图标")?;
    *app.state::<RuntimeState>().tray_icon.lock() = Some(tray);
    eprintln!("AlphaKey menu bar item registered: redkey-tray");
    Ok(())
}

fn toggle_quick_panel_inner(app: &AppHandle) -> Result<()> {
    let quick = app
        .get_webview_window("quick-panel")
        .context("快捷面板窗口不存在")?;
    if quick.is_visible()? {
        quick.hide()?;
        return Ok(());
    }
    show_quick_panel_inner(app, true)
}

fn show_quick_panel_inner(app: &AppHandle, focus: bool) -> Result<()> {
    if app.state::<RuntimeState>().hover_regions.lock().dragging {
        return Ok(());
    }
    let quick = app
        .get_webview_window("quick-panel")
        .context("快捷面板窗口不存在")?;
    // The window-state plugin may restore an old size, so reset this transient panel before positioning it.
    quick.set_size(Size::Logical(LogicalSize::new(320.0, 420.0)))?;
    if let Some(pet) = app.get_webview_window("pet") {
        let pet_position = pet.outer_position()?;
        let pet_size = pet.outer_size()?;
        let quick_size = quick.outer_size()?;
        let monitor = pet
            .current_monitor()?
            .or(pet.primary_monitor()?)
            .context("无法读取当前显示器")?;
        let work_area = monitor.work_area();
        let left = work_area.position.x;
        let top = work_area.position.y;
        let right = left + work_area.size.width as i32;
        let bottom = top + work_area.size.height as i32;
        let left_candidate = pet_position.x - quick_size.width as i32 - 4;
        let right_candidate = pet_position.x + pet_size.width as i32 + 4;
        let mut x = if left_candidate >= left {
            left_candidate
        } else {
            right_candidate
        };
        let mut y = pet_position.y + (pet_size.height as i32 - quick_size.height as i32) / 2;
        x = x.clamp(left, (right - quick_size.width as i32).max(left));
        y = y.clamp(top, (bottom - quick_size.height as i32).max(top));
        quick.set_position(Position::Physical(PhysicalPosition::new(x, y)))?;
    }
    quick.show()?;
    quick.emit("redkey://quick-panel-shown", ())?;
    if focus {
        quick.set_focus()?;
    }
    Ok(())
}

fn schedule_hover_hide(app: &AppHandle, generation: u64) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(120));
        let should_hide = {
            let state = app.state::<RuntimeState>();
            let regions = state.hover_regions.lock();
            regions.generation == generation && !regions.pet && !regions.panel
        };
        if should_hide {
            if let Some(panel) = app.get_webview_window("quick-panel") {
                let _ = panel.hide();
            }
        }
    });
}

fn inside_window(app: &AppHandle, label: &str, cursor_x: f64, cursor_y: f64) -> Result<bool> {
    let Some(window) = app.get_webview_window(label) else {
        return Ok(false);
    };
    if !window.is_visible().unwrap_or(false) {
        return Ok(false);
    }
    let position = window.outer_position()?;
    let size = window.outer_size()?;
    Ok(cursor_x >= position.x as f64
        && cursor_y >= position.y as f64
        && cursor_x < (position.x + size.width as i32) as f64
        && cursor_y < (position.y + size.height as i32) as f64)
}

#[tauri::command]
fn sync_hover_state(app: AppHandle) -> Result<(), String> {
    let cursor = app.cursor_position().map_err(err)?;
    let pet = inside_window(&app, "pet", cursor.x, cursor.y).map_err(err)?;
    let panel = inside_window(&app, "quick-panel", cursor.x, cursor.y).map_err(err)?;
    let panel_visible = app
        .get_webview_window("quick-panel")
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false);
    let (show, hide_generation) = {
        let state = app.state::<RuntimeState>();
        let mut regions = state.hover_regions.lock();
        let changed = regions.pet != pet || regions.panel != panel;
        let show = !regions.dragging && pet && (!regions.pet || !panel_visible);
        regions.pet = pet;
        regions.panel = panel;
        if changed {
            regions.generation = regions.generation.wrapping_add(1);
        }
        let hide = if changed && !pet && !panel {
            Some(regions.generation)
        } else {
            None
        };
        (show, hide)
    };
    if show {
        show_quick_panel_inner(&app, false).map_err(err)?;
    }
    if let Some(generation) = hide_generation {
        schedule_hover_hide(&app, generation);
    }
    Ok(())
}

#[tauri::command]
fn set_pet_dragging(app: AppHandle, dragging: bool) -> Result<(), String> {
    let should_hide = {
        let state = app.state::<RuntimeState>();
        let mut regions = state.hover_regions.lock();
        if regions.dragging == dragging {
            false
        } else {
            regions.dragging = dragging;
            regions.generation = regions.generation.wrapping_add(1);
            if !dragging {
                // Force the next cursor check to decide whether the panel should return.
                regions.pet = false;
                regions.panel = false;
            }
            dragging
        }
    };
    if should_hide {
        if let Some(panel) = app.get_webview_window("quick-panel") {
            panel.hide().map_err(err)?;
        }
    }
    if !dragging {
        sync_hover_state(app)?;
    }
    Ok(())
}

#[tauri::command]
fn set_pet_mode(app: AppHandle, mode: String) -> Result<(), String> {
    let valid_modes = ["default", "edit", "recording", "ai-summary"];
    if !valid_modes.contains(&mode.as_str()) {
        return Err(format!("Invalid pet mode: {}", mode));
    }
    {
        let state = app.state::<RuntimeState>();
        let mut pet_mode = state.pet_mode.lock();
        *pet_mode = mode.clone();
    }
    if let Some(pet_window) = app.get_webview_window("pet") {
        let _ = pet_window.emit("redkey://pet-mode", mode);
    }
    Ok(())
}

#[tauri::command]
fn activate_slot(app: AppHandle, slot: i64) -> Result<(), String> {
    dispatch_internal(&app, AppAction::ActivateSlot { slot }).map_err(err)?;
    Ok(())
}

#[tauri::command]
fn get_snapshot(app: AppHandle) -> Result<Snapshot, String> {
    snapshot(&app).map_err(err)
}

#[tauri::command]
fn get_task_document(app: AppHandle, task_id: String) -> Result<TaskDocument, String> {
    app.state::<RuntimeState>().db().task_document(&task_id).map_err(err)
}

#[tauri::command]
fn create_text_card(app: AppHandle, task_id: String) -> Result<TextCard, String> {
    let card = app.state::<RuntimeState>().db().create_text_card(&task_id, "manual").map_err(err)?;
    let _ = emit_snapshot(&app);
    Ok(card)
}

#[tauri::command]
fn update_text_card(app: AppHandle, card_id: String, content: String) -> Result<(), String> {
    app.state::<RuntimeState>().db().update_text_card(&card_id, &content).map_err(err)
}

#[tauri::command]
fn delete_text_card(app: AppHandle, card_id: String) -> Result<(), String> {
    app.state::<RuntimeState>().db().delete_text_card(&card_id).map_err(err)?;
    let _ = emit_snapshot(&app);
    Ok(())
}

#[tauri::command]
fn reassign_text_card(app: AppHandle, card_id: String, task_id: String) -> Result<Snapshot, String> {
    app.state::<RuntimeState>().db().reassign_text_card(&card_id, &task_id).map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn paste_text_card(app: AppHandle, task_id: String) -> Result<TextCard, String> {
    let text = app.clipboard().read_text().map_err(err)?.trim().to_string();
    if text.is_empty() { return Err("剪切板为空".into()); }
    if text.chars().count() > 50_000 { return Err("剪切板文本过长".into()); }
    let card = app.state::<RuntimeState>().db().create_text_card(&task_id, "manual").map_err(err)?;
    app.state::<RuntimeState>().db().update_text_card(&card.id, &text).map_err(err)?;
    let timestamp = chrono::Utc::now().to_rfc3339();
    let card = TextCard { id: card.id, task_id: card.task_id, content: text, source: "manual".into(), created_at: timestamp.clone(), updated_at: timestamp };
    let _ = emit_snapshot(&app);
    Ok(card)
}

#[tauri::command]
fn create_image_card(app: AppHandle, task_id: String, filename: String, mime_type: String, data: String, content: String) -> Result<ImageCard, String> {
    let card = app.state::<RuntimeState>().db().create_image_card(&task_id, &filename, &mime_type, &data, &content).map_err(err)?;
    let _ = emit_snapshot(&app);
    Ok(card)
}

#[tauri::command]
fn update_image_card(app: AppHandle, card_id: String, filename: String, mime_type: String, data: String, content: String) -> Result<(), String> {
    app.state::<RuntimeState>().db().update_image_card(&card_id, &filename, &mime_type, &data, &content).map_err(err)?;
    let _ = emit_snapshot(&app);
    Ok(())
}

#[tauri::command]
fn ocr_image_card(app: AppHandle, card_id: String) -> Result<String, String> {
    use base64::Engine;
    let card = app.state::<RuntimeState>().db().get_image_card(&card_id).map_err(err)?;
    if card.data.is_empty() { return Err("图片卡还没有图片".into()); }
    let bytes = base64::engine::general_purpose::STANDARD.decode(&card.data).map_err(|e| format!("图片数据解码失败：{e}"))?;
    let ext = if card.mime_type == "image/png" { "png" } else { "jpg" };
    let tmp = std::env::temp_dir().join(format!("redkey_ocr_{}.{}", card.id, ext));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("写入临时文件失败：{e}"))?;
    let text = crate::ocr::OcrWorker::start(&app).and_then(|mut worker| worker.ocr(&tmp)).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&tmp);
    app.state::<RuntimeState>().db().update_image_card(&card.id, &card.filename, &card.mime_type, &card.data, &text).map_err(err)?;
    let _ = emit_snapshot(&app);
    Ok(text)
}

#[tauri::command]
fn delete_image_card(app: AppHandle, card_id: String) -> Result<(), String> {
    app.state::<RuntimeState>().db().delete_image_card(&card_id).map_err(err)?;
    let _ = emit_snapshot(&app);
    Ok(())
}

#[tauri::command]
fn reassign_image_card(app: AppHandle, card_id: String, task_id: String) -> Result<Snapshot, String> {
    app.state::<RuntimeState>().db().reassign_image_card(&card_id, &task_id).map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn update_task_title(app: AppHandle, task_id: String, title: String) -> Result<Snapshot, String> {
    app.state::<RuntimeState>().db().update_task_title(&task_id, &title).map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn update_task_contact(app: AppHandle, task_id: String, contact_id: Option<String>) -> Result<Snapshot, String> {
    app.state::<RuntimeState>().db().update_task_contact(&task_id, contact_id.as_deref()).map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn update_task_link(app: AppHandle, task_id: String, url: Option<String>) -> Result<Snapshot, String> {
    app.state::<RuntimeState>().db().update_task_link(&task_id, url.as_deref()).map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn delete_completed_task(app: AppHandle, task_id: String) -> Result<Snapshot, String> {
    let paths = app.state::<RuntimeState>().db().task_document(&task_id).map_err(err)?.recordings.into_iter().filter_map(|recording| recording.audio_path).collect::<Vec<_>>();
    app.state::<RuntimeState>().db().delete_completed_task(&task_id).map_err(err)?;
    for path in paths { let _ = std::fs::remove_file(path); }
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn clear_all_data(app: AppHandle) -> Result<Snapshot, String> {
    if app.state::<RuntimeState>().native_recording.lock().is_some() || app.state::<RuntimeState>().db().snapshot().map_err(err)?.recordings.iter().any(|recording| recording.status == "recording") {
        return Err("录音进行中，无法清除数据".into());
    }
    app.state::<RuntimeState>().db().clear_all_data().map_err(err)?;
    let settings = app.state::<RuntimeState>().db().settings().map_err(err)?;
    if settings.autostart { app.autolaunch().enable().map_err(|error| format!("无法恢复开机启动：{error}"))?; }
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn resolve_task_overflow(app: AppHandle, keep_ids: Vec<String>) -> Result<Snapshot, String> {
    app.state::<RuntimeState>().db().resolve_task_overflow(&keep_ids).map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn get_deepseek_settings() -> Result<DeepSeekSettings, String> { llm::settings().map_err(err) }

#[tauri::command]
fn save_deepseek_api_key(api_key: String) -> Result<DeepSeekSettings, String> {
    llm::save_key(&api_key).map_err(err)?;
    llm::settings().map_err(err)
}

#[tauri::command]
fn delete_deepseek_api_key() -> Result<DeepSeekSettings, String> {
    llm::delete_key().map_err(err)?;
    llm::settings().map_err(err)
}

#[tauri::command]
async fn test_deepseek_connection() -> Result<(), String> { llm::test_connection().await.map_err(err) }

#[tauri::command]
async fn summarize_task(app: AppHandle, task_id: String) -> Result<TextCard, String> {
    let document = app.state::<RuntimeState>().db().task_document(&task_id).map_err(err)?;
    let summary = llm::summarize_task(&document).await.map_err(err)?;
    let card = app.state::<RuntimeState>().db().create_text_card(&task_id, "ai").map_err(err)?;
    app.state::<RuntimeState>().db().update_text_card(&card.id, &summary).map_err(err)?;
    let _ = emit_snapshot(&app);
    Ok(TextCard { content: summary, ..card })
}

#[tauri::command]
fn summarize_recording(app: AppHandle, recording_id: String) -> Result<(), String> {
    let document = app.state::<RuntimeState>().db().task_document_for_recording(&recording_id).map_err(err)?;
    let recording = document.recordings.iter().find(|recording| recording.id == recording_id).ok_or("录音记录不存在")?;
    if recording.transcript.trim().is_empty() && recording.raw_transcript.trim().is_empty() { return Err("录音尚未完成转写".into()); }
    spawn_recording_summary_with_force(&app, &recording_id, true);
    Ok(())
}

#[tauri::command]
fn get_task_summary_prompt(app: AppHandle, task_id: String) -> Result<String, String> {
    let document = app.state::<RuntimeState>().db().task_document(&task_id).map_err(err)?;
    llm::task_summary_prompt(&document).map_err(err)
}

#[tauri::command]
fn get_recording_summary_prompt(app: AppHandle, recording_id: String) -> Result<String, String> {
    let document = app.state::<RuntimeState>().db().task_document_for_recording(&recording_id).map_err(err)?;
    llm::recording_summary_prompt(&document, &recording_id).map_err(err)
}

#[tauri::command]
fn retry_recording_summary(app: AppHandle, recording_id: String) -> Result<(), String> { summarize_recording(app, recording_id) }

#[tauri::command]
fn update_recording_summary(app: AppHandle, recording_id: String, mut summary: RecordingSummary) -> Result<(), String> {
    if summary.recording_id != recording_id { return Err("录音总结归属不匹配".into()); }
    summary.status = "completed".into();
    summary.error_message = None;
    summary.user_edited = true;
    summary.updated_at = chrono::Utc::now().to_rfc3339();
    app.state::<RuntimeState>().db().save_recording_summary(&summary).map_err(err)?;
    let _ = emit_snapshot(&app);
    Ok(())
}

#[tauri::command]
fn keyboard_listener_status(app: AppHandle) -> Option<String> {
    #[cfg(any(target_os = "macos", windows))]
    {
        app.state::<RuntimeState>().keyboard_monitor.lock().as_ref().and_then(|monitor| monitor.error.lock().clone())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = app;
        None
    }
}

#[tauri::command]
fn create_task(app: AppHandle, input: CreateTaskInput) -> Result<Snapshot, String> {
    app.state::<RuntimeState>()
        .db()
        .create_task(input)
        .map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn update_task(app: AppHandle, input: UpdateTaskInput) -> Result<Snapshot, String> {
    app.state::<RuntimeState>()
        .db()
        .update_task(input)
        .map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn delete_task(app: AppHandle, task_id: String) -> Result<Snapshot, String> {
    app.state::<RuntimeState>()
        .db()
        .delete_task(&task_id)
        .map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn set_current_task(app: AppHandle, task_id: String, open: bool) -> Result<Snapshot, String> {
    let url = app
        .state::<RuntimeState>()
        .db()
        .set_current_task(&task_id)
        .map_err(err)?;
    if open && !url.trim().is_empty() {
        open_link(&url).map_err(err)?;
    }
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn bind_slot(app: AppHandle, group: String, slot: i64, task_id: Option<String>) -> Result<Snapshot, String> {
    app.state::<RuntimeState>()
        .db()
        .bind_slot(&group, slot, task_id.as_deref())
        .map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn swap_slots(app: AppHandle, group: String, slot_a: i64, slot_b: i64) -> Result<Snapshot, String> {
    app.state::<RuntimeState>()
        .db()
        .swap_slots(&group, slot_a, slot_b)
        .map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn set_current_group(app: AppHandle, group: String) -> Result<Snapshot, String> {
    app.state::<RuntimeState>().db().set_current_group(&group).map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn set_group_name(app: AppHandle, group: String, name: String) -> Result<Snapshot, String> {
    app.state::<RuntimeState>().db().set_group_name(&group, &name).map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn move_task_to_top(app: AppHandle, task_id: String) -> Result<Snapshot, String> {
    app.state::<RuntimeState>()
        .db()
        .move_task_to_top(&task_id)
        .map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn add_contact(app: AppHandle, name: String) -> Result<Snapshot, String> {
    app.state::<RuntimeState>()
        .db()
        .add_contact(&name)
        .map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn rename_contact(app: AppHandle, contact_id: String, name: String) -> Result<Snapshot, String> {
    app.state::<RuntimeState>()
        .db()
        .rename_contact(&contact_id, &name)
        .map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn remove_contact(app: AppHandle, contact_id: String) -> Result<Snapshot, String> {
    app.state::<RuntimeState>()
        .db()
        .remove_contact(&contact_id)
        .map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn dispatch_action(app: AppHandle, action: AppAction) -> Result<Snapshot, String> {
    dispatch_internal(&app, action).map_err(err)
}

#[tauri::command]
fn update_settings(app: AppHandle, settings: Settings) -> Result<Snapshot, String> {
    settings.validate().map_err(err)?;
    let previous = app
        .state::<RuntimeState>()
        .db()
        .settings()
        .map_err(err)?;
    update_keyboard_listener(&app, &settings.shortcuts).map_err(err)?;
    if settings.autostart != previous.autostart {
        let launcher = app.autolaunch();
        let changed = if settings.autostart {
            launcher.enable()
        } else {
            launcher.disable()
        };
        if let Err(error) = changed {
            let _ = update_keyboard_listener(&app, &previous.shortcuts);
            return Err(format!("无法修改开机启动：{error}"));
        }
    }
    app.state::<RuntimeState>()
        .db()
        .save_settings(&settings)
        .map_err(err)?;
    if settings.pet_visible != previous.pet_visible {
        let pet = app.get_webview_window("pet").ok_or("宠物窗口不存在").map_err(err)?;
        if settings.pet_visible { pet.show().map_err(err)?; } else { pet.hide().map_err(err)?; }
    }
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<Snapshot, String> {
    let previous = app
        .state::<RuntimeState>()
        .db()
        .settings()
        .map_err(err)?;
    let mut settings = previous.clone();
    settings.autostart = enabled;
    settings.validate().map_err(err)?;
    if settings.autostart != previous.autostart {
        let launcher = app.autolaunch();
        let changed = if settings.autostart {
            launcher.enable()
        } else {
            launcher.disable()
        };
        changed.map_err(|error| format!("无法修改开机启动：{error}"))?;
    }
    app.state::<RuntimeState>()
        .db()
        .save_settings(&settings)
        .map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn set_pet_visible(app: AppHandle, visible: bool) -> Result<Snapshot, String> {
    set_pet_visible_inner(&app, visible).map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn save_shortcuts(app: AppHandle, shortcuts: ShortcutSettings) -> Result<Snapshot, String> {
    let previous = app
        .state::<RuntimeState>()
        .db()
        .settings()
        .map_err(err)?;
    let settings = Settings {
        autostart: previous.autostart,
        pet_visible: previous.pet_visible,
        multi_group_enabled: previous.multi_group_enabled,
        cloud_api_enabled: previous.cloud_api_enabled,
        shortcuts,
    };
    settings.validate().map_err(err)?;
    update_keyboard_listener(&app, &settings.shortcuts).map_err(err)?;
    app.state::<RuntimeState>()
        .db()
        .save_settings(&settings)
        .map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn start_recording(app: AppHandle) -> Result<String, String> {
    let snapshot = app.state::<RuntimeState>().db().snapshot().map_err(err)?;
    let current = snapshot.current_task_id.as_deref()
        .and_then(|id| snapshot.tasks.iter().find(|task| task.id == id && task.status == "active"));
    let task = current.or_else(|| {
        let mut candidates: Vec<&Task> = snapshot.tasks.iter()
            .filter(|task| task.status == "active" && task.slot.is_some())
            .collect();
        candidates.sort_by(|a, b| b.last_opened_at.cmp(&a.last_opened_at));
        candidates.into_iter().next()
    });
    let task_id = task.map(|task| task.id.as_str()).ok_or_else(|| "没有绑定按键的任务，无法录音".to_string())?;
    let id = app.state::<RuntimeState>().db().start_recording(Some(task_id)).map_err(err)?;
    emit_snapshot(&app).map_err(err)?;
    Ok(id)
}

#[tauri::command]
async fn request_microphone_permission() -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        return tauri::async_runtime::spawn_blocking(|| {
            // The request must originate from RedKey.app's main thread. Polling happens
            // on a worker so the macOS consent sheet can remain responsive.
            unsafe { redkey_request_microphone_permission() };
            for _ in 0..600 {
                match unsafe { redkey_microphone_authorization_status() } {
                    3 => return Ok(true),
                    1 | 2 => return Ok(false),
                    _ => std::thread::sleep(Duration::from_millis(100)),
                }
            }
            Err("等待麦克风授权超时，请重新打开 AlphaKey 后再试".into())
        })
        .await
        .map_err(err)?;
    }
    #[cfg(not(target_os = "macos"))]
    Ok(true)
}

fn prepare_native_recording(app: &AppHandle) -> Result<(String, std::path::PathBuf), String> {
    if app.state::<RuntimeState>().native_recording.lock().is_some() { return Err("已经在录音".into()); }
    let id = start_recording(app.clone())?;
    let dir = app.path().app_data_dir().map_err(err)?.join("recordings");
    std::fs::create_dir_all(&dir).map_err(err)?;
    let path = dir.join(format!("{id}.wav"));
    Ok((id, path))
}

fn finalize_native_recording(app: &AppHandle, recording: NativeRecording) -> String {
    let id = recording.id.clone();
    *app.state::<RuntimeState>().native_recording.lock() = Some(recording);
    id
}

#[tauri::command]
fn start_native_recording(app: AppHandle) -> Result<String, String> {
    #[cfg(not(any(target_os = "macos", windows)))]
    { return Err("当前版本的原生录音暂只支持 macOS 和 Windows".into()); }
    #[cfg(target_os = "macos")]
    {
        let (id, path) = prepare_native_recording(&app)?;
        let mut child = match Command::new(env!("REDKEY_AUDIO_HELPER")).arg(&path).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = app.state::<RuntimeState>().db().fail_recording(&id, &format!("音频进程启动失败：{e}"));
                return Err(format!("音频进程启动失败：{e}"));
            }
        };
        let input = child.stdin.take().ok_or("无法连接录音进程")?;
        let stdout = child.stdout.take().ok_or("无法读取录音进程")?;
        let mut reader = BufReader::new(stdout);
        let mut ready = String::new();
        reader.read_line(&mut ready).map_err(err)?;
        if ready.trim() != "READY" {
            let _ = child.kill();
            app.state::<RuntimeState>().db().fail_recording(&id, "无法启动系统麦克风").map_err(err)?;
            return Err("无法启动系统麦克风，请检查麦克风权限".into());
        }
        Ok(finalize_native_recording(&app, NativeRecording { id: id.clone(), path, started: std::time::Instant::now(), child, input }))
    }
    #[cfg(windows)]
    {
        let (id, path) = prepare_native_recording(&app)?;
        let recording = match recording_windows::start_recording(id.clone(), path.clone()) {
            Ok(r) => r,
            Err(e) => {
                let _ = app.state::<RuntimeState>().db().fail_recording(&id, &format!("麦克风启动失败：{e}"));
                return Err(format!("麦克风启动失败：{e}"));
            }
        };
        Ok(finalize_native_recording(&app, recording))
    }
}

#[tauri::command]
fn stop_native_recording(app: AppHandle) -> Result<Snapshot, String> {
    let mut recording = app.state::<RuntimeState>().native_recording.lock().take().ok_or("当前没有录音")?;
    #[cfg(target_os = "macos")]
    {
        writeln!(recording.input, "stop").map_err(err)?;
        recording.input.flush().map_err(err)?;
        let status = recording.child.wait().map_err(err)?;
        if !status.success() {
            app.state::<RuntimeState>().db().fail_recording(&recording.id, "原生录音进程异常退出").map_err(err)?;
            return Err("录音保存失败".into());
        }
    }
    #[cfg(windows)]
    {
        recording.stop().map_err(err)?;
    }
    let duration = recording.started.elapsed().as_secs_f64();
    app.state::<RuntimeState>().db().finish_recording(&recording.id, duration, &recording.path.to_string_lossy()).map_err(err)?;
    emit_snapshot(&app).map_err(err)?;
    begin_processing(&app, &recording.id);
    let background_app = app.clone();
    std::thread::spawn(move || run_transcription_pipeline(background_app, recording.path, recording.id));
    snapshot(&app).map_err(err)
}

#[tauri::command]
fn native_recording_level(app: AppHandle) -> Result<f32, String> {
    let state = app.state::<RuntimeState>();
    let recording = state.native_recording.lock();
    #[cfg(windows)]
    {
        if let Some(rec) = recording.as_ref() {
            return Ok(rec.level());
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = recording;
        return Ok(0.0);
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = recording;
        return Ok(0.0);
    }
    Err("当前没有录音".into())
}

#[tauri::command]
fn finish_recording(app: AppHandle, recording_id: String, audio: Vec<u8>, duration: f64, _transcript: String) -> Result<Snapshot, String> {
    let recordings_dir = app.path().app_data_dir().map_err(err)?.join("recordings");
    std::fs::create_dir_all(&recordings_dir).map_err(err)?;
    let path = recordings_dir.join(format!("{recording_id}.wav"));
    std::fs::write(&path, audio).map_err(err)?;
    app.state::<RuntimeState>().db().finish_recording(&recording_id, duration, &path.to_string_lossy()).map_err(err)?;
    emit_snapshot(&app).map_err(err)?;
    begin_processing(&app, &recording_id);
    let background_app = app.clone();
    std::thread::spawn(move || run_transcription_pipeline(background_app, path, recording_id));
    snapshot(&app).map_err(err)
}

#[tauri::command]
fn fail_recording(app: AppHandle, recording_id: String, message: String) -> Result<Snapshot, String> {
    app.state::<RuntimeState>().db().fail_recording(&recording_id, &message).map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn delete_recording(app: AppHandle, recording_id: String) -> Result<Snapshot, String> {
    cancel_processing(&app, &recording_id);
    let path = app.path().app_data_dir().map_err(err)?.join("recordings").join(format!("{recording_id}.wav"));
    let _ = std::fs::remove_file(path);
    app.state::<RuntimeState>().db().delete_recording(&recording_id).map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn retry_transcription(app: AppHandle, recording_id: String) -> Result<Snapshot, String> {
    let recording = snapshot(&app).map_err(err)?.recordings.into_iter().find(|item| item.id == recording_id).ok_or("录音记录不存在")?;
    let path = recording.audio_path.map(std::path::PathBuf::from).ok_or("录音文件不存在")?;
    app.state::<RuntimeState>().db().finish_recording(&recording_id, recording.duration, &path.to_string_lossy()).map_err(err)?;
    begin_processing(&app, &recording_id);
    let background_app = app.clone();
    std::thread::spawn(move || run_transcription_pipeline(background_app, path, recording_id));
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn reassign_recording(app: AppHandle, recording_id: String, task_id: Option<String>) -> Result<Snapshot, String> {
    app.state::<RuntimeState>().db().reassign_recording(&recording_id, task_id.as_deref()).map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn get_recording_detail(app: AppHandle, recording_id: String) -> Result<RecordingDetail, String> { app.state::<RuntimeState>().db().recording_detail(&recording_id).map_err(err) }

#[tauri::command]
fn recording_audio_data(app: AppHandle, recording_id: String) -> Result<Vec<u8>, String> { let detail = app.state::<RuntimeState>().db().recording_detail(&recording_id).map_err(err)?; let path = detail.recording.audio_path.ok_or("录音文件不存在")?; std::fs::read(path).map_err(err) }

#[tauri::command]
fn export_data(app: AppHandle) -> Result<String, String> {
    let bundle = app
        .state::<RuntimeState>()
        .db()
        .export()
        .map_err(err)?;
    serde_json::to_string_pretty(&bundle).map_err(err)
}

#[tauri::command]
fn import_data(app: AppHandle, payload: String) -> Result<Snapshot, String> {
    let bundle: ExportBundle =
        serde_json::from_str(&payload).map_err(|error| format!("备份 JSON 无效：{error}"))?;
    let settings = bundle.settings.clone();
    update_keyboard_listener(&app, &settings.shortcuts).map_err(err)?;
    app.state::<RuntimeState>()
        .db()
        .import(bundle)
        .map_err(err)?;
    emit_snapshot(&app).map_err(err)
}

#[tauri::command]
fn toggle_quick_panel(app: AppHandle) -> Result<(), String> {
    toggle_quick_panel_inner(&app).map_err(err)
}

#[tauri::command]
fn show_quick_panel(app: AppHandle) -> Result<(), String> {
    show_quick_panel_inner(&app, false).map_err(err)
}

#[tauri::command]
fn submit_dropped_link(app: AppHandle, url: String) -> Result<(), String> {
    let url = url.trim();
    let parsed = url::Url::parse(url).map_err(|_| "拖入的内容不是有效链接".to_string())?;
    if !["http", "https"].contains(&parsed.scheme()) {
        return Err("只支持 HTTP 或 HTTPS 链接".into());
    }
    show_quick_panel_inner(&app, false).map_err(err)?;
    app.emit("redkey://link-drop", url).map_err(err)
}

#[tauri::command]
fn show_console(app: AppHandle) -> Result<(), String> {
    show_console_window(&app).map_err(err)
}

#[tauri::command]
fn open_console_new_task(app: AppHandle, url: String) -> Result<(), String> {
    let url = url.trim();
    let parsed = url::Url::parse(url).map_err(|_| "链接格式无效".to_string())?;
    if !["http", "https"].contains(&parsed.scheme()) {
        return Err("只支持 HTTP 或 HTTPS 链接".into());
    }
    show_console_window(&app).map_err(err)?;
    if let Some(quick) = app.get_webview_window("quick-panel") {
        let _ = quick.hide();
    }
    app.emit("redkey://new-task", url).map_err(err)
}

#[tauri::command]
async fn resolve_link_title(url: String) -> Result<TitleSuggestion, String> {
    let parsed = url::Url::parse(url.trim()).map_err(|_| "链接格式无效".to_string())?;
    if !["http", "https"].contains(&parsed.scheme()) {
        return Err("只支持 HTTP 或 HTTPS 链接".into());
    }
    let slug = parsed
        .path_segments()
        .and_then(|segments| segments.filter(|segment| !segment.is_empty()).last())
        .map(|segment| humanize_title(&urlencoding::decode(segment).unwrap_or_default()))
        .filter(|value| !value.is_empty());
    let is_figma = parsed
        .host_str()
        .is_some_and(|host| host == "figma.com" || host.ends_with(".figma.com"));
    if is_figma {
        let endpoint = format!(
            "https://www.figma.com/api/oembed?url={}",
            urlencoding::encode(url.trim())
        );
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .user_agent("RedKey/0.1")
            .build()
            .map_err(err)?;
        if let Ok(response) = client.get(endpoint).send().await {
            if let Ok(value) = response.json::<serde_json::Value>().await {
                if let Some(title) = value
                    .get("title")
                    .and_then(|title| title.as_str())
                    .map(humanize_title)
                    .filter(|value| !value.is_empty())
                {
                    return Ok(TitleSuggestion {
                        source_title: title.clone(),
                        suggested_title: title,
                        source: "metadata".into(),
                    });
                }
            }
        }
    }
    let title = slug.unwrap_or_else(|| parsed.host_str().unwrap_or("未命名任务").to_string());
    let source = if is_figma { "url" } else { "fallback" };
    Ok(TitleSuggestion {
        source_title: title.clone(),
        suggested_title: title,
        source: source.into(),
    })
}

fn humanize_title(value: &str) -> String {
    value
        .replace(['-', '_'], " ")
        .replace(" | Figma", "")
        .replace(" – Figma", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .manage(RuntimeState::default())
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            let _ = show_console_window(app);
        }))
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                // The HUD's visibility is purely a transient reflection of the
                // shortcut prefix key being held; persisting/restoring it across
                // launches can leave it shown (and, on Windows, blocking clicks
                // underneath it) even before any key is pressed.
                .skip_initial_state("hud")
                .build(),
        )
        .plugin({
            tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--background"]))
        })
        .setup(|app| {
            let data_dir = app.path().app_data_dir().context("无法获取应用数据目录")?;
            let database = match Database::open(&data_dir.join("redkey.sqlite3")) {
                Ok(db) => {
                    eprintln!("Database opened successfully from {:?}", data_dir.join("redkey.sqlite3"));
                    db
                }
                Err(e) => {
                    eprintln!("Failed to open database: {e}");
                    Database::memory().context("无法创建内存数据库")?
                }
            };
            let settings = database.settings().unwrap_or_default();
            if settings.autostart { let _ = app.autolaunch().enable(); }
            *app.state::<RuntimeState>().db.lock() = Some(database);
            eprintln!("Database initialized, emitting snapshot");
            let _ = emit_snapshot(app.handle());
            eprintln!("Snapshot emitted successfully");
            if let Err(e) = update_keyboard_listener(app.handle(), &settings.shortcuts) {
                eprintln!("Failed to start keyboard listener: {e}");
            }
            if let Err(e) = setup_tray(app) {
                eprintln!("Failed to setup tray: {e}");
            }
            if !settings.pet_visible {
                if let Some(pet) = app.get_webview_window("pet") { let _ = pet.hide(); }
            }
            // Defensive reset: the HUD should always start hidden and click-through.
            // Without this, a HUD left visible from an unclean previous exit (or a
            // platform where nothing sets ignore-cursor-events at show time) would
            // block mouse input to whatever sits underneath its bounds.
            if let Some(hud) = app.get_webview_window("hud") {
                let _ = hud.set_ignore_cursor_events(true);
                let _ = hud.hide();
            }
            // Force non-overlay scrollbar on macOS WebKit
            if let Some(window) = app.get_webview_window("console") { let _ = window.eval("document.documentElement.style.scrollbarGutter='stable'"); }
            if std::env::args().any(|argument| argument == "--background") {
                if let Some(console) = app.get_webview_window("console") {
                    let _ = console.hide();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::CloseRequested { api, .. } if window.label() == "console" => {
                api.prevent_close();
                let _ = window.hide();
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            get_task_document,
            create_text_card,
            update_text_card,
            delete_text_card,
            reassign_text_card,
            paste_text_card,
            create_image_card,
            update_image_card,
            ocr_image_card,
            delete_image_card,
            reassign_image_card,
            update_task_title,
            update_task_contact,
            update_task_link,
            delete_completed_task,
            clear_all_data,
            resolve_task_overflow,
            get_deepseek_settings,
            save_deepseek_api_key,
            delete_deepseek_api_key,
            test_deepseek_connection,
            summarize_task,
            summarize_recording,
            retry_recording_summary,
            update_recording_summary,
            get_task_summary_prompt,
            get_recording_summary_prompt,
            keyboard_listener_status,
            create_task,
            update_task,
            delete_task,
            set_current_task,
            set_current_group,
            set_group_name,
            bind_slot,
            swap_slots,
            move_task_to_top,
            add_contact,
            rename_contact,
            remove_contact,
            dispatch_action,
            activate_slot,
            update_settings,
            set_autostart,
            set_pet_visible,
            save_shortcuts,
            request_microphone_permission,
            start_recording,
            start_native_recording,
            stop_native_recording,
            native_recording_level,
            finish_recording,
            fail_recording,
            delete_recording,
            reassign_recording,
            get_recording_detail,
            recording_audio_data,
            retry_transcription,
            export_data,
            import_data,
            toggle_quick_panel,
            show_quick_panel,
            set_pet_dragging,
            set_pet_mode,
            sync_hover_state,
            submit_dropped_link,
            show_console,
            open_console_new_task,
            resolve_link_title,
            crate::ocr::perform_ocr,
        ]);

    let app = builder
        .build(tauri::generate_context!())
        .expect("failed to build RedKey");
    app.run(|_app, event| if matches!(event, RunEvent::ExitRequested { .. }) {});
}

#[cfg(test)]
mod tests {
    use super::humanize_title;

    #[test]
    fn cleans_url_titles() {
        assert_eq!(humanize_title("login-page_redesign"), "login page redesign");
        assert_eq!(humanize_title("Login Page | Figma"), "Login Page");
    }
}
