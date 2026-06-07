//! Text Inserter using Windows SendInput API
//!
//! Inserts text into the currently focused window using keyboard simulation.

use anyhow::{anyhow, Result};
use std::mem::size_of;
use std::thread;
use std::time::{Duration, Instant};

use crate::data::TextInsertionConfig;

#[cfg(target_os = "windows")]
use windows::core::w;
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
#[cfg(target_os = "windows")]
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
#[cfg(target_os = "windows")]
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
#[cfg(target_os = "windows")]
const CF_UNICODETEXT_FORMAT: u32 = 13;
#[cfg(target_os = "windows")]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_V,
};
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WS_CHILD,
};

#[cfg(not(target_os = "windows"))]
#[derive(Clone, Copy)]
struct INPUT;

#[cfg(not(target_os = "windows"))]
#[derive(Clone, Copy)]
struct VIRTUAL_KEY;

#[cfg(not(target_os = "windows"))]
const VK_BACK: VIRTUAL_KEY = VIRTUAL_KEY;

/// Text inserter service using Windows SendInput API
pub struct TextInserter {
    config: TextInsertionConfig,
}

impl TextInserter {
    /// Create a new text inserter with default settings.
    pub fn new() -> Self {
        Self::with_config(TextInsertionConfig::default())
    }

    /// Create a new text inserter with explicit insertion settings.
    pub fn with_config(config: TextInsertionConfig) -> Self {
        Self { config }
    }

    /// Insert text with the configured fast path.
    pub fn insert_fast(&self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        let mode = self.config.mode.to_ascii_lowercase();
        let should_clipboard = mode == "clipboard"
            || (mode == "auto" && text.chars().count() >= self.config.clipboard_threshold_chars);

        if should_clipboard {
            match self.insert_via_clipboard(text) {
                Ok(()) => return Ok(()),
                Err(e) if mode == "clipboard" => return Err(e),
                Err(e) => {
                    tracing::warn!("Clipboard insert failed, falling back to SendInput: {}", e)
                }
            }
        }

        self.insert(text)
    }

    /// Insert text into the currently focused window using Unicode SendInput.
    pub fn insert(&self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        let started_at = Instant::now();
        let mut inputs: Vec<INPUT> = Vec::new();

        for ch in text.encode_utf16() {
            // Key down
            inputs.push(self.create_unicode_input(ch, true));
            // Key up
            inputs.push(self.create_unicode_input(ch, false));
        }

        self.send_inputs(&inputs)?;
        tracing::debug!(
            "Inserted {} chars via SendInput in {:.1} ms",
            text.chars().count(),
            started_at.elapsed().as_secs_f64() * 1000.0
        );
        Ok(())
    }

    /// Delete specified number of characters (simulate backspace)
    pub fn delete_chars(&self, count: usize) -> Result<()> {
        if count == 0 {
            return Ok(());
        }

        let mut inputs: Vec<INPUT> = Vec::new();

        for _ in 0..count {
            // Backspace key down
            inputs.push(self.create_key_input(VK_BACK, true));
            // Backspace key up
            inputs.push(self.create_key_input(VK_BACK, false));
        }

        self.send_inputs(&inputs)?;
        Ok(())
    }

    #[cfg(target_os = "windows")]
    fn insert_via_clipboard(&self, text: &str) -> Result<()> {
        let started_at = Instant::now();
        let previous_clipboard = read_clipboard_text().ok().flatten();

        write_clipboard_text(text)?;
        // Give the target app a short chance to observe the new clipboard
        // contents before receiving Ctrl+V. Some Windows apps defer clipboard
        // reads until their message loop handles the paste shortcut.
        thread::sleep(Duration::from_millis(30));
        self.send_ctrl_v()?;

        if self.config.clipboard_restore_delay_ms > 0 {
            thread::sleep(Duration::from_millis(
                self.config.clipboard_restore_delay_ms,
            ));
        }

        if let Some(previous_text) = previous_clipboard {
            if let Err(e) = write_clipboard_text(&previous_text) {
                tracing::warn!("Failed to restore clipboard text: {}", e);
            }
        }

        tracing::debug!(
            "Inserted {} chars via clipboard paste in {:.1} ms",
            text.chars().count(),
            started_at.elapsed().as_secs_f64() * 1000.0
        );
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    fn insert_via_clipboard(&self, _text: &str) -> Result<()> {
        Err(anyhow!("clipboard insertion is only available on Windows"))
    }

    #[cfg(target_os = "windows")]
    fn send_ctrl_v(&self) -> Result<()> {
        let inputs = [
            self.create_key_input(VK_CONTROL, true),
            self.create_key_input(VK_V, true),
            self.create_key_input(VK_V, false),
            self.create_key_input(VK_CONTROL, false),
        ];
        self.send_inputs(&inputs)
    }

    /// Create a Unicode character input
    #[cfg(target_os = "windows")]
    fn create_unicode_input(&self, ch: u16, key_down: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: ch,
                    dwFlags: if key_down {
                        KEYEVENTF_UNICODE
                    } else {
                        KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn create_unicode_input(&self, _ch: u16, _key_down: bool) -> INPUT {
        INPUT
    }

    /// Create a virtual key input
    #[cfg(target_os = "windows")]
    fn create_key_input(&self, vk: VIRTUAL_KEY, key_down: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if key_down {
                        windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(0)
                    } else {
                        KEYEVENTF_KEYUP
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn create_key_input(&self, _vk: VIRTUAL_KEY, _key_down: bool) -> INPUT {
        INPUT
    }

    /// Send inputs using Windows SendInput API
    #[cfg(target_os = "windows")]
    fn send_inputs(&self, inputs: &[INPUT]) -> Result<()> {
        if inputs.is_empty() {
            return Ok(());
        }

        let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };

        if sent != inputs.len() as u32 {
            return Err(anyhow!(
                "SendInput sent {} of {} inputs; target may be elevated or blocked",
                sent,
                inputs.len()
            ));
        }

        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    fn send_inputs(&self, _inputs: &[INPUT]) -> Result<()> {
        Err(anyhow!("text insertion is only available on Windows"))
    }
}

#[cfg(target_os = "windows")]
fn hglobal_from_handle(handle: HANDLE) -> HGLOBAL {
    HGLOBAL(handle.0 as *mut std::ffi::c_void)
}

#[cfg(target_os = "windows")]
fn handle_from_hglobal(hglobal: HGLOBAL) -> HANDLE {
    HANDLE(hglobal.0 as isize)
}

#[cfg(target_os = "windows")]
fn read_clipboard_text() -> Result<Option<String>> {
    let _guard = ClipboardGuard::open(HWND(0))?;
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT_FORMAT) };
    let handle = match handle {
        Ok(handle) if !handle.is_invalid() => handle,
        _ => return Ok(None),
    };

    let ptr = unsafe { GlobalLock(hglobal_from_handle(handle)) } as *const u16;
    if ptr.is_null() {
        return Ok(None);
    }

    let mut len = 0usize;
    unsafe {
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        let text = String::from_utf16_lossy(slice);
        let _ = GlobalUnlock(hglobal_from_handle(handle));
        Ok(Some(text))
    }
}

#[cfg(target_os = "windows")]
fn write_clipboard_text(text: &str) -> Result<()> {
    let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_len = utf16.len() * size_of::<u16>();

    // EmptyClipboard requires a real owner window before SetClipboardData can succeed.
    let owner = ClipboardOwnerWindow::new()?;
    let _guard = ClipboardGuard::open(owner.hwnd())?;

    let hglobal = unsafe { GlobalAlloc(GMEM_MOVEABLE, byte_len)? };
    let ptr = unsafe { GlobalLock(hglobal) } as *mut u16;
    if ptr.is_null() {
        unsafe {
            let _ = GlobalFree(hglobal);
        }
        return Err(anyhow!("GlobalLock failed for clipboard buffer"));
    }

    unsafe {
        std::ptr::copy_nonoverlapping(utf16.as_ptr(), ptr, utf16.len());
        let _ = GlobalUnlock(hglobal);
    }

    unsafe {
        if let Err(e) = EmptyClipboard() {
            let _ = GlobalFree(hglobal);
            return Err(e.into());
        }

        if let Err(e) = SetClipboardData(CF_UNICODETEXT_FORMAT, handle_from_hglobal(hglobal)) {
            let _ = GlobalFree(hglobal);
            return Err(e.into());
        }
    }

    Ok(())
}

#[cfg(target_os = "windows")]
struct ClipboardOwnerWindow(HWND);

#[cfg(target_os = "windows")]
impl ClipboardOwnerWindow {
    fn new() -> Result<Self> {
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("DoubaoClipboardOwner"),
                WS_CHILD,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                None,
                None,
                None,
            )
        };

        if hwnd.0 == 0 {
            return Err(anyhow!("failed to create clipboard owner window"));
        }

        Ok(Self(hwnd))
    }

    fn hwnd(&self) -> HWND {
        self.0
    }
}

#[cfg(target_os = "windows")]
impl Drop for ClipboardOwnerWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

#[cfg(target_os = "windows")]
struct ClipboardGuard;

#[cfg(target_os = "windows")]
impl ClipboardGuard {
    fn open(owner: HWND) -> Result<Self> {
        unsafe { OpenClipboard(owner)? };
        Ok(Self)
    }
}

#[cfg(target_os = "windows")]
impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

impl Default for TextInserter {
    fn default() -> Self {
        Self::new()
    }
}
