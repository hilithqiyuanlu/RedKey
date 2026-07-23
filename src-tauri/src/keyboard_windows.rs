use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use crate::models::AppAction;

#[derive(Clone, Copy)]
pub struct PrefixConfig {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub caps: bool,
}

impl Default for PrefixConfig {
    fn default() -> Self {
        Self { control: true, alt: true, shift: false, win: false, caps: false }
    }
}

impl PrefixConfig {
    pub fn from_string(value: &str) -> Self {
        let mut config = Self { control: false, alt: false, shift: false, win: false, caps: false };
        for item in value.split('+').map(str::trim) {
            match item {
                "Control" => config.control = true,
                "Alt" | "Option" => config.alt = true,
                "Shift" => config.shift = true,
                "Command" | "Win" | "Windows" => config.win = true,
                "CapsLock" => config.caps = true,
                _ => {}
            }
        }
        config
    }
}

pub enum KeyboardEvent {
    Prefix(bool),
    Action(AppAction),
}

fn read_modifier_state() -> PrefixConfig {
    unsafe {
        PrefixConfig {
            control: (GetAsyncKeyState(VK_CONTROL as i32) as u16 & 0x8000) != 0,
            alt: (GetAsyncKeyState(VK_MENU as i32) as u16 & 0x8000) != 0,
            shift: (GetAsyncKeyState(VK_SHIFT as i32) as u16 & 0x8000) != 0,
            win: (GetAsyncKeyState(VK_LWIN as i32) as u16 & 0x8000) != 0
                || (GetAsyncKeyState(VK_RWIN as i32) as u16 & 0x8000) != 0,
            caps: (GetKeyState(VK_CAPITAL as i32) as u16 & 0x0001) != 0,
        }
    }
}

fn exact_prefix_match(config: &PrefixConfig, modifiers: &PrefixConfig) -> bool {
    config.control == modifiers.control
        && config.alt == modifiers.alt
        && config.shift == modifiers.shift
        && config.win == modifiers.win
        && config.caps == modifiers.caps
}

fn is_key_down(vk: i32) -> bool {
    unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
}

fn vk_for_slot(slot: usize) -> i32 {
    match slot {
        0 => 0x31, 1 => 0x32, 2 => 0x33, 3 => 0x34, 4 => 0x35,
        5 => 0x36, 6 => 0x37, 7 => 0x38, 8 => 0x39, 9 => 0x30,
        _ => 0,
    }
}

pub struct KeyboardMonitor {
    pub config: Arc<Mutex<PrefixConfig>>,
    pub error: Arc<Mutex<Option<String>>>,
    pub running: Arc<AtomicBool>,
    stop_signal: mpsc::Sender<()>,
}

unsafe impl Send for KeyboardMonitor {}
unsafe impl Sync for KeyboardMonitor {}

pub fn start_keyboard_monitor(
    app: &AppHandle,
    settings: &crate::models::ShortcutSettings,
) -> KeyboardMonitor {
    let config = Arc::new(Mutex::new(PrefixConfig::from_string(
        &settings.task_prefix,
    )));
    let error = Arc::new(Mutex::new(None));
    let running = Arc::new(AtomicBool::new(false));
    let (event_sender, event_receiver) = mpsc::channel();
    let (stop_sender, stop_receiver) = mpsc::channel::<()>();
    let app_handle = app.clone();

    // ---- event processing thread ----
    std::thread::spawn(move || {
        for event in event_receiver {
            match event {
                KeyboardEvent::Prefix(active) => {
                    let _ = crate::set_task_hud_visible(&app_handle, active);
                    let _ = app_handle.emit("redkey://prefix-changed", active);
                }
                KeyboardEvent::Action(AppAction::ActivateSlot { slot }) => {
                    let _ = crate::dispatch_internal(
                        &app_handle,
                        AppAction::ActivateSlot { slot },
                    );
                }
                KeyboardEvent::Action(action) => {
                    let _ = crate::dispatch_internal(&app_handle, action);
                }
            }
        }
    });

    let running_clone = running.clone();
    let config_for_monitor = config.clone();
    let event_sender_monitor = event_sender.clone();

    // ---- monitor thread: polls modifier + action keys at ~50 Hz ----
    std::thread::spawn(move || {
        running_clone.store(true, Ordering::SeqCst);

        // Ensure this thread has a message queue so GetAsyncKeyState works
        // reliably (PeekMessage creates one if absent).
        let mut dummy_msg = unsafe { std::mem::zeroed::<MSG>() };
        unsafe { PeekMessageW(&mut dummy_msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE); }

        let mut prefix_active = false;

        // Track previous key-down state so we only fire on the rising edge.
        let mut prev_slot_keys = [false; 10];
        let mut prev_t_key = false;

        const VK_T: i32 = 0x54;

        loop {
            // ---- check stop signal (non-blocking) ----
            if stop_receiver.try_recv().is_ok() {
                break;
            }

            // ---- read current modifier mask ----
            let current_mods = read_modifier_state();
            let config = *config_for_monitor.lock();
            let is_prefix = exact_prefix_match(&config, &current_mods);

            if is_prefix != prefix_active {
                prefix_active = is_prefix;
                let _ = event_sender_monitor.send(KeyboardEvent::Prefix(is_prefix));
            }

            if is_prefix {
                // digit keys 0-9
                for slot in 0..10 {
                    let pressed = is_key_down(vk_for_slot(slot));
                    if pressed && !prev_slot_keys[slot] {
                        let _ = event_sender_monitor.send(KeyboardEvent::Action(
                            AppAction::ActivateSlot {
                                slot: slot as i64,
                            },
                        ));
                    }
                    prev_slot_keys[slot] = pressed;
                }

                // 'T' → toggle recording
                let t_down = is_key_down(VK_T);
                if t_down && !prev_t_key {
                    let _ = event_sender_monitor
                        .send(KeyboardEvent::Action(AppAction::ToggleRecording));
                }
                prev_t_key = t_down;
            } else {
                prev_slot_keys = [false; 10];
                prev_t_key = false;
            }

            std::thread::sleep(Duration::from_millis(20));
        }

        running_clone.store(false, Ordering::SeqCst);
    });

    KeyboardMonitor {
        config,
        error,
        running,
        stop_signal: stop_sender,
    }
}

impl KeyboardMonitor {
    pub fn update_config(&self, new_config: PrefixConfig) {
        *self.config.lock() = new_config;
    }
}
