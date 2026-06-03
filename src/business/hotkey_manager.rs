//! Hotkey Manager
//!
//! Manages global hotkeys for triggering voice input.
//! Supports combo keys (Ctrl+Shift+V), double-tap of modifier keys (Ctrl), and tap/hold keys (Fn).

use anyhow::{anyhow, Result};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::data::HotkeyConfig;

/// Hotkey mode
#[derive(Debug, Clone, PartialEq)]
pub enum HotkeyMode {
    /// Combination key mode (e.g., Ctrl+Shift+V)
    Combo,
    /// Double-tap mode (e.g., double-tap Ctrl)
    DoubleTap,
    /// Tap/hold mode (e.g., tap Fn to toggle, hold Fn to record until released)
    TapHold,
}

/// Event emitted by a hotkey action
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// Toggle voice input on or off
    Toggle,
    /// Start voice input
    Start,
    /// Stop voice input
    Stop,
    /// Finish a quick tap in tap/hold mode
    Tap,
}

/// Hotkey manager for global hotkey handling
pub struct HotkeyManager {
    _manager: Option<GlobalHotKeyManager>,
    mode: HotkeyMode,
    double_tap_interval: Duration,
    double_tap_key: String,
    tap_hold_key: String,
    tap_hold_threshold: Duration,
    is_active: Arc<AtomicBool>,
}

impl HotkeyManager {
    /// Create a new hotkey manager based on configuration
    pub fn new(config: &HotkeyConfig) -> Result<Self> {
        let mode = match config.mode.as_str() {
            "combo" => HotkeyMode::Combo,
            "double_tap" => HotkeyMode::DoubleTap,
            "tap_hold" => HotkeyMode::TapHold,
            other => {
                tracing::warn!("Unknown hotkey mode '{}', falling back to tap_hold", other);
                HotkeyMode::TapHold
            }
        };

        let manager = GlobalHotKeyManager::new()
            .map_err(|e| anyhow!("Failed to create hotkey manager: {}", e))?;

        // Register hotkey based on mode
        match mode {
            HotkeyMode::Combo => {
                // Parse combo key (default: Ctrl+Shift+V)
                let hotkey = parse_combo_key(&config.combo_key)?;
                manager
                    .register(hotkey)
                    .map_err(|e| anyhow!("Failed to register hotkey: {}", e))?;
                tracing::info!("Registered combo hotkey: {}", config.combo_key);
            }
            HotkeyMode::DoubleTap => {
                // For modifier keys like Ctrl, we use low-level keyboard hook.
                // For regular keys, we can use global_hotkey.
                let key_lower = config.double_tap_key.to_lowercase();
                if key_lower == "ctrl" || key_lower == "shift" || key_lower == "alt" {
                    // Will use Windows keyboard hook for modifier keys.
                    tracing::info!(
                        "Double-tap modifier key: {} (using keyboard hook)",
                        config.double_tap_key
                    );
                } else {
                    // Regular key - can use global_hotkey.
                    let hotkey = HotKey::new(None, parse_key_code(&config.double_tap_key)?);
                    manager
                        .register(hotkey)
                        .map_err(|e| anyhow!("Failed to register hotkey: {}", e))?;
                    tracing::info!("Registered double-tap hotkey: {}", config.double_tap_key);
                }
            }
            HotkeyMode::TapHold => {
                // Tap/hold needs key-down and key-up events, so it is handled by
                // the Windows low-level keyboard hook instead of global_hotkey.
                tracing::info!(
                    "Tap/hold key: {} (tap toggles, hold records until release)",
                    config.tap_hold_key
                );
            }
        }

        Ok(Self {
            _manager: Some(manager),
            mode,
            double_tap_interval: Duration::from_millis(config.double_tap_interval),
            double_tap_key: config.double_tap_key.clone(),
            tap_hold_key: config.tap_hold_key.clone(),
            tap_hold_threshold: Duration::from_millis(config.tap_hold_threshold),
            is_active: Arc::new(AtomicBool::new(true)),
        })
    }

    /// Set callback for when a toggle-style hotkey is triggered.
    pub fn on_trigger<F>(&self, callback: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.on_event(move |event| {
            if event == HotkeyEvent::Toggle {
                callback();
            }
        });
    }

    /// Set callback for when a hotkey event is emitted.
    pub fn on_event<F>(&self, callback: F)
    where
        F: Fn(HotkeyEvent) + Send + Sync + 'static,
    {
        let mode = self.mode.clone();
        let double_tap_interval = self.double_tap_interval;
        let double_tap_key = self.double_tap_key.clone();
        let tap_hold_key = self.tap_hold_key.clone();
        let tap_hold_threshold = self.tap_hold_threshold;
        let is_active = self.is_active.clone();
        let callback: Arc<dyn Fn(HotkeyEvent) + Send + Sync> = Arc::new(callback);

        // Check if we need to use keyboard hook for modifier keys
        let key_lower = double_tap_key.to_lowercase();
        let use_keyboard_hook = mode == HotkeyMode::DoubleTap
            && (key_lower == "ctrl" || key_lower == "shift" || key_lower == "alt");

        if mode == HotkeyMode::TapHold {
            #[cfg(target_os = "windows")]
            {
                thread::spawn(move || {
                    run_tap_hold_hook(tap_hold_key, tap_hold_threshold, is_active, callback);
                });
            }
            #[cfg(not(target_os = "windows"))]
            {
                tracing::warn!("Tap/hold hotkey mode is only supported on Windows");
            }
        } else if use_keyboard_hook {
            // Use Windows keyboard hook for modifier key double-tap
            #[cfg(target_os = "windows")]
            {
                let callback_clone = callback.clone();
                thread::spawn(move || {
                    run_modifier_double_tap_hook(
                        key_lower,
                        double_tap_interval,
                        is_active,
                        callback_clone,
                    );
                });
            }
            #[cfg(not(target_os = "windows"))]
            {
                tracing::warn!("Modifier key double-tap not supported on this platform");
            }
        } else {
            // Use global_hotkey receiver
            thread::spawn(move || {
                let receiver = GlobalHotKeyEvent::receiver();
                let mut last_press_time: Option<Instant> = None;

                loop {
                    if !is_active.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    }

                    if let Ok(_event) = receiver.recv() {
                        match mode {
                            HotkeyMode::Combo => {
                                callback(HotkeyEvent::Toggle);
                            }
                            HotkeyMode::DoubleTap => {
                                let now = Instant::now();

                                if let Some(last) = last_press_time {
                                    let elapsed = now.duration_since(last);
                                    if elapsed <= double_tap_interval {
                                        callback(HotkeyEvent::Toggle);
                                        last_press_time = None;
                                        continue;
                                    }
                                }

                                last_press_time = Some(now);
                            }
                            HotkeyMode::TapHold => unreachable!("tap/hold uses keyboard hook"),
                        }
                    }
                }
            });
        }
    }

    /// Stop the hotkey manager
    pub fn stop(&self) {
        self.is_active.store(false, Ordering::SeqCst);
    }
}

/// Windows keyboard hook for modifier key double-tap detection
#[cfg(target_os = "windows")]
fn run_modifier_double_tap_hook(
    key: String,
    interval: Duration,
    is_active: Arc<AtomicBool>,
    callback: Arc<dyn Fn(HotkeyEvent) + Send + Sync>,
) {
    use std::cell::RefCell;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_RCONTROL, VK_RMENU, VK_RSHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
        HHOOK, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYUP, WM_SYSKEYUP,
    };

    // Determine which virtual keys to watch
    let target_vks: Vec<u16> = match key.as_str() {
        "ctrl" => vec![VK_CONTROL.0, VK_LCONTROL.0, VK_RCONTROL.0],
        "shift" => vec![VK_LSHIFT.0, VK_RSHIFT.0],
        "alt" => vec![VK_LMENU.0, VK_RMENU.0],
        _ => vec![],
    };

    if target_vks.is_empty() {
        tracing::error!("Unknown modifier key: {}", key);
        return;
    }

    tracing::info!("Starting keyboard hook for double-tap {} detection", key);

    // Thread-local state for hook callback
    thread_local! {
        static HOOK_STATE: RefCell<Option<HookState>> = RefCell::new(None);
    }

    struct HookState {
        target_vks: Vec<u16>,
        interval: Duration,
        last_release: Option<Instant>,
        callback: Arc<dyn Fn(HotkeyEvent) + Send + Sync>,
        is_active: Arc<AtomicBool>,
    }

    // Initialize thread-local state
    HOOK_STATE.with(|state| {
        *state.borrow_mut() = Some(HookState {
            target_vks,
            interval,
            last_release: None,
            callback,
            is_active,
        });
    });

    // Low-level keyboard hook procedure
    unsafe extern "system" fn keyboard_hook_proc(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if code >= 0 {
            let kb_struct = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            let vk_code = kb_struct.vkCode as u16;
            let is_key_up = wparam.0 as u32 == WM_KEYUP || wparam.0 as u32 == WM_SYSKEYUP;

            HOOK_STATE.with(|state| {
                if let Some(ref mut hook_state) = *state.borrow_mut() {
                    if hook_state.is_active.load(Ordering::SeqCst)
                        && hook_state.target_vks.contains(&vk_code)
                        && is_key_up
                    {
                        let now = Instant::now();
                        if let Some(last) = hook_state.last_release {
                            let elapsed = now.duration_since(last);
                            if elapsed <= hook_state.interval {
                                // Double-tap detected!
                                tracing::info!("Double-tap detected!");
                                (hook_state.callback)(HotkeyEvent::Toggle);
                                hook_state.last_release = None;
                            } else {
                                hook_state.last_release = Some(now);
                            }
                        } else {
                            hook_state.last_release = Some(now);
                        }
                    }
                }
            });
        }

        CallNextHookEx(HHOOK::default(), code, wparam, lparam)
    }

    // Install the hook
    let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), None, 0) };

    match hook {
        Ok(h) => {
            tracing::info!("Keyboard hook installed successfully");

            // Message loop to keep hook alive
            let mut msg = MSG::default();
            unsafe {
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    DispatchMessageW(&msg);
                }
            }

            // Cleanup
            let _ = unsafe { UnhookWindowsHookEx(h) };
            tracing::info!("Keyboard hook uninstalled");
        }
        Err(e) => {
            tracing::error!("Failed to install keyboard hook: {:?}", e);
        }
    }
}

/// Windows keyboard hook for tap/hold detection.
///
/// A key-down always emits `Start`. If the key is released quickly, it emits
/// `Tap` so the caller can keep a new recording running or stop an existing one.
/// If the key is held longer than `hold_threshold`, releasing it emits `Stop`.
#[cfg(target_os = "windows")]
fn run_tap_hold_hook(
    key: String,
    hold_threshold: Duration,
    is_active: Arc<AtomicBool>,
    callback: Arc<dyn Fn(HotkeyEvent) + Send + Sync>,
) {
    use std::cell::RefCell;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
        HHOOK, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN,
        WM_SYSKEYUP,
    };

    let target = match parse_tap_hold_target(&key) {
        Ok(target) => target,
        Err(e) => {
            tracing::error!("Invalid tap/hold key '{}': {}", key, e);
            return;
        }
    };

    tracing::info!("Starting keyboard hook for tap/hold {} detection", key);

    thread_local! {
        static HOOK_STATE: RefCell<Option<TapHoldHookState>> = RefCell::new(None);
    }

    struct TapHoldHookState {
        target: TapHoldTarget,
        hold_threshold: Duration,
        press_time: Option<Instant>,
        is_pressed: bool,
        callback: Arc<dyn Fn(HotkeyEvent) + Send + Sync>,
        is_active: Arc<AtomicBool>,
    }

    HOOK_STATE.with(|state| {
        *state.borrow_mut() = Some(TapHoldHookState {
            target,
            hold_threshold,
            press_time: None,
            is_pressed: false,
            callback,
            is_active,
        });
    });

    unsafe extern "system" fn keyboard_hook_proc(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if code >= 0 {
            let kb_struct = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            let vk_code = kb_struct.vkCode as u16;
            let scan_code = kb_struct.scanCode;
            let message = wparam.0 as u32;
            let is_key_down = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
            let is_key_up = message == WM_KEYUP || message == WM_SYSKEYUP;

            HOOK_STATE.with(|state| {
                if let Some(ref mut hook_state) = *state.borrow_mut() {
                    if !hook_state.is_active.load(Ordering::SeqCst)
                        || !hook_state.target.matches(vk_code, scan_code)
                    {
                        return;
                    }

                    if is_key_down && !hook_state.is_pressed {
                        hook_state.is_pressed = true;
                        hook_state.press_time = Some(Instant::now());
                        tracing::info!("Tap/hold key pressed; starting voice input");
                        (hook_state.callback)(HotkeyEvent::Start);
                    } else if is_key_up && hook_state.is_pressed {
                        hook_state.is_pressed = false;
                        let held_for = hook_state
                            .press_time
                            .take()
                            .map(|pressed_at| Instant::now().duration_since(pressed_at))
                            .unwrap_or_default();

                        if held_for >= hook_state.hold_threshold {
                            tracing::info!(
                                "Tap/hold key released after hold; stopping voice input"
                            );
                            (hook_state.callback)(HotkeyEvent::Stop);
                        } else {
                            tracing::info!("Tap/hold key tapped");
                            (hook_state.callback)(HotkeyEvent::Tap);
                        }
                    }
                }
            });
        }

        CallNextHookEx(HHOOK::default(), code, wparam, lparam)
    }

    let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), None, 0) };

    match hook {
        Ok(h) => {
            tracing::info!("Tap/hold keyboard hook installed successfully");

            let mut msg = MSG::default();
            unsafe {
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    DispatchMessageW(&msg);
                }
            }

            let _ = unsafe { UnhookWindowsHookEx(h) };
            tracing::info!("Tap/hold keyboard hook uninstalled");
        }
        Err(e) => {
            tracing::error!("Failed to install tap/hold keyboard hook: {:?}", e);
        }
    }
}

#[cfg(target_os = "windows")]
#[derive(Debug, Clone)]
struct TapHoldTarget {
    virtual_keys: Vec<u16>,
    scan_codes: Vec<u32>,
}

#[cfg(target_os = "windows")]
impl TapHoldTarget {
    fn from_virtual_keys(virtual_keys: Vec<u16>) -> Self {
        Self {
            virtual_keys,
            scan_codes: Vec::new(),
        }
    }

    fn matches(&self, vk_code: u16, scan_code: u32) -> bool {
        self.virtual_keys.contains(&vk_code) || self.scan_codes.contains(&scan_code)
    }
}

#[cfg(target_os = "windows")]
fn parse_tap_hold_target(key: &str) -> Result<TapHoldTarget> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        VK_CONTROL, VK_ESCAPE, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_MENU, VK_RCONTROL, VK_RMENU,
        VK_RSHIFT, VK_SHIFT, VK_SPACE,
    };

    let key_upper = key.trim().to_uppercase();
    let virtual_keys = match key_upper.as_str() {
        "FN" => {
            // Windows does not define a standard VK_FN. Some keyboard firmware/drivers
            // still expose standalone Fn presses to WH_KEYBOARD_LL as VK 0xFF, F23/F24,
            // or the common Fn scan code 0x63. Match all known exposed forms so the
            // default Fn tap/hold key works whenever Windows receives an Fn event.
            return Ok(TapHoldTarget {
                virtual_keys: vec![0xFF, 0x86, 0x87],
                scan_codes: vec![0x63],
            });
        }
        "CTRL" | "CONTROL" => vec![VK_CONTROL.0, VK_LCONTROL.0, VK_RCONTROL.0],
        "SHIFT" => vec![VK_SHIFT.0, VK_LSHIFT.0, VK_RSHIFT.0],
        "ALT" => vec![VK_MENU.0, VK_LMENU.0, VK_RMENU.0],
        "SPACE" => vec![VK_SPACE.0],
        "ESC" | "ESCAPE" => vec![VK_ESCAPE.0],
        "ENTER" | "RETURN" => vec![0x0D],
        key if key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric() => {
            vec![key.as_bytes()[0] as u16]
        }
        key if key.starts_with('F') => {
            let number = key[1..]
                .parse::<u16>()
                .map_err(|_| anyhow!("Unknown tap/hold key: {}", key))?;
            if (1..=24).contains(&number) {
                vec![0x70 + number - 1]
            } else {
                return Err(anyhow!("Function key out of range: {}", key));
            }
        }
        _ => return Err(anyhow!("Unknown tap/hold key: {}", key)),
    };

    Ok(TapHoldTarget::from_virtual_keys(virtual_keys))
}

/// Parse a combo key string like "Ctrl+Shift+V"
fn parse_combo_key(key_str: &str) -> Result<HotKey> {
    let parts: Vec<&str> = key_str.split('+').map(|s| s.trim()).collect();

    let mut modifiers = Modifiers::empty();
    let mut key_code: Option<Code> = None;

    for part in parts {
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "shift" => modifiers |= Modifiers::SHIFT,
            "alt" => modifiers |= Modifiers::ALT,
            "super" | "win" | "meta" => modifiers |= Modifiers::SUPER,
            _ => {
                key_code = Some(parse_key_code(part)?);
            }
        }
    }

    let code = key_code.ok_or_else(|| anyhow!("No key specified in combo: {}", key_str))?;

    Ok(HotKey::new(Some(modifiers), code))
}

/// Parse a key code from string
fn parse_key_code(key: &str) -> Result<Code> {
    let code = match key.to_uppercase().as_str() {
        "A" => Code::KeyA,
        "B" => Code::KeyB,
        "C" => Code::KeyC,
        "D" => Code::KeyD,
        "E" => Code::KeyE,
        "F" => Code::KeyF,
        "G" => Code::KeyG,
        "H" => Code::KeyH,
        "I" => Code::KeyI,
        "J" => Code::KeyJ,
        "K" => Code::KeyK,
        "L" => Code::KeyL,
        "M" => Code::KeyM,
        "N" => Code::KeyN,
        "O" => Code::KeyO,
        "P" => Code::KeyP,
        "Q" => Code::KeyQ,
        "R" => Code::KeyR,
        "S" => Code::KeyS,
        "T" => Code::KeyT,
        "U" => Code::KeyU,
        "V" => Code::KeyV,
        "W" => Code::KeyW,
        "X" => Code::KeyX,
        "Y" => Code::KeyY,
        "Z" => Code::KeyZ,
        "0" => Code::Digit0,
        "1" => Code::Digit1,
        "2" => Code::Digit2,
        "3" => Code::Digit3,
        "4" => Code::Digit4,
        "5" => Code::Digit5,
        "6" => Code::Digit6,
        "7" => Code::Digit7,
        "8" => Code::Digit8,
        "9" => Code::Digit9,
        "SPACE" => Code::Space,
        "ENTER" | "RETURN" => Code::Enter,
        "ESCAPE" | "ESC" => Code::Escape,
        "F1" => Code::F1,
        "F2" => Code::F2,
        "F3" => Code::F3,
        "F4" => Code::F4,
        "F5" => Code::F5,
        "F6" => Code::F6,
        "F7" => Code::F7,
        "F8" => Code::F8,
        "F9" => Code::F9,
        "F10" => Code::F10,
        "F11" => Code::F11,
        "F12" => Code::F12,
        _ => return Err(anyhow!("Unknown key: {}", key)),
    };

    Ok(code)
}
