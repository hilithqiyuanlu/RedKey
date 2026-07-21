use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
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

struct SharedState {
    // Read on every keystroke system-wide, written only when the user changes
    // the shortcut prefix in settings: an RwLock lets the hot path take a
    // cheap read lock instead of contending on a plain Mutex.
    config: RwLock<PrefixConfig>,
    // Track modifier state locally instead of reading GetAsyncKeyState, because
    // the hook runs before the system updates key state, so key-up events would
    // still read as "pressed" and the HUD would not dismiss on release.
    modifiers: Mutex<PrefixConfig>,
    // Never replaced after the monitor starts, so it doesn't need a Mutex —
    // mpsc::Sender is already Clone + Send + Sync.
    sender: mpsc::Sender<KeyboardEvent>,
}

static mut SHARED_STATE: Option<Arc<SharedState>> = None;

fn vk_to_action(vk: u32) -> Option<AppAction> {
    match vk {
        v if v == 0x31 => Some(AppAction::ActivateSlot { slot: 0 }),
        v if v == 0x32 => Some(AppAction::ActivateSlot { slot: 1 }),
        v if v == 0x33 => Some(AppAction::ActivateSlot { slot: 2 }),
        v if v == 0x34 => Some(AppAction::ActivateSlot { slot: 3 }),
        v if v == 0x35 => Some(AppAction::ActivateSlot { slot: 4 }),
        v if v == 0x36 => Some(AppAction::ActivateSlot { slot: 5 }),
        v if v == 0x37 => Some(AppAction::ActivateSlot { slot: 6 }),
        v if v == 0x38 => Some(AppAction::ActivateSlot { slot: 7 }),
        v if v == 0x39 => Some(AppAction::ActivateSlot { slot: 8 }),
        v if v == 0x30 => Some(AppAction::ActivateSlot { slot: 9 }),
        _ => None,
    }
}

fn vk_modifier_kind(vk: u32) -> Option<fn(&mut PrefixConfig, bool)> {
    match vk {
        v if v == VK_CONTROL as u32
            || v == VK_LCONTROL as u32
            || v == VK_RCONTROL as u32 => Some(|m, v| m.control = v),
        v if v == VK_MENU as u32
            || v == VK_LMENU as u32
            || v == VK_RMENU as u32 => Some(|m, v| m.alt = v),
        v if v == VK_SHIFT as u32
            || v == VK_LSHIFT as u32
            || v == VK_RSHIFT as u32 => Some(|m, v| m.shift = v),
        v if v == VK_LWIN as u32 || v == VK_RWIN as u32 => Some(|m, v| m.win = v),
        v if v == VK_CAPITAL as u32 => Some(|m, v| m.caps = v),
        _ => None,
    }
}

fn exact_prefix_match(config: &PrefixConfig, modifiers: &PrefixConfig) -> bool {
    config.control == modifiers.control
        && config.alt == modifiers.alt
        && config.shift == modifiers.shift
        && config.win == modifiers.win
        && config.caps == modifiers.caps
}

unsafe extern "system" fn keyboard_proc(code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, w_param, l_param);
    }

    let kbd_struct = *(l_param as *const KBDLLHOOKSTRUCT);
    let vk = kbd_struct.vkCode;
    let is_down = matches!(w_param as u32, WM_KEYDOWN | WM_SYSKEYDOWN);

    let shared = unsafe { &*core::ptr::addr_of_mut!(SHARED_STATE) };
    if let Some(state) = shared {
        let config = *state.config.read();
        if let Some(update_modifier) = vk_modifier_kind(vk) {
            // 修饰键：先更新本地状态，再判断前缀是否匹配
            let mut modifiers = state.modifiers.lock();
            update_modifier(&mut modifiers, is_down);
            let active = exact_prefix_match(&config, &modifiers);
            drop(modifiers);
            let _ = state.sender.send(KeyboardEvent::Prefix(active));
        } else if is_down {
            // 非修饰键且按下：检查前缀是否匹配，匹配则触发动作并吞键
            let modifiers = *state.modifiers.lock();
            if exact_prefix_match(&config, &modifiers) {
                if let Some(action) = vk_to_action(vk) {
                    let _ = state.sender.send(KeyboardEvent::Action(action));
                    return 1;
                }
            }
        }
    }

    CallNextHookEx(std::ptr::null_mut(), code, w_param, l_param)
}

pub struct KeyboardMonitor {
    pub config: Arc<Mutex<PrefixConfig>>,
    pub error: Arc<Mutex<Option<String>>>,
    pub running: Arc<AtomicBool>,
    shared_state: Arc<SharedState>,
}

unsafe impl Send for KeyboardMonitor {}
unsafe impl Sync for KeyboardMonitor {}

pub fn install_keyboard_tap(_app: &AppHandle, monitor: &KeyboardMonitor) {
    if monitor.running.swap(true, Ordering::SeqCst) { return; }

    let running = monitor.running.clone();
    let error = monitor.error.clone();
    let shared_state = monitor.shared_state.clone();

    std::thread::spawn(move || {
        unsafe {
            SHARED_STATE = Some(shared_state.clone());

            let hinst = GetModuleHandleW(std::ptr::null());
            let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), hinst, 0);

            if hook.is_null() {
                *error.lock() = Some("无法安装全局键盘钩子".into());
                running.store(false, Ordering::SeqCst);
                SHARED_STATE = None;
                return;
            }

            let mut msg = std::mem::zeroed::<MSG>();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                if msg.message == WM_QUIT {
                    break;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            UnhookWindowsHookEx(hook);
            SHARED_STATE = None;
            running.store(false, Ordering::SeqCst);
        }
    });
}

pub fn start_keyboard_monitor(app: &AppHandle, settings: &crate::models::ShortcutSettings) -> KeyboardMonitor {
    let config = Arc::new(Mutex::new(PrefixConfig::from_string(&settings.task_prefix)));
    let error = Arc::new(Mutex::new(None));
    let running = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel();
    let app_handle = app.clone();

    let shared_state = Arc::new(SharedState {
        config: RwLock::new(PrefixConfig::from_string(&settings.task_prefix)),
        modifiers: Mutex::new(PrefixConfig { control: false, alt: false, shift: false, win: false, caps: false }),
        sender,
    });

    std::thread::spawn(move || {
        for event in receiver {
            match event {
                KeyboardEvent::Prefix(active) => {
                    let _ = crate::set_task_hud_visible(&app_handle, active);
                    let _ = app_handle.emit("redkey://prefix-changed", active);
                }
                KeyboardEvent::Action(AppAction::ActivateSlot { slot }) => {
                    let _ = crate::dispatch_internal(&app_handle, AppAction::ActivateSlot { slot });
                }
                KeyboardEvent::Action(action) => {
                    let _ = crate::dispatch_internal(&app_handle, action);
                }
            }
        }
    });

    let monitor = KeyboardMonitor {
        config: config.clone(),
        error,
        running,
        shared_state,
    };
    install_keyboard_tap(app, &monitor);
    monitor
}

impl KeyboardMonitor {
    pub fn update_config(&self, new_config: PrefixConfig) {
        *self.config.lock() = new_config;
        if let Some(state) = unsafe { &*core::ptr::addr_of_mut!(SHARED_STATE) } {
            *state.config.write() = new_config;
        }
    }
}
