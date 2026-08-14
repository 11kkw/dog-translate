use std::{thread, time::Duration};

#[cfg(target_os = "windows")]
use std::mem::size_of;

#[cfg(target_os = "macos")]
use std::ffi::c_void;

use anyhow::{Context, Result, bail};
use arboard::Clipboard;

#[cfg(target_os = "windows")]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_C,
    VK_CONTROL,
};

#[cfg(target_os = "macos")]
const MAC_KEY_C: u16 = 8;
#[cfg(target_os = "macos")]
const MAC_COMMAND_FLAG: u64 = 1 << 20;
#[cfg(target_os = "macos")]
const MAC_HID_EVENT_TAP: u32 = 0;

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventCreateKeyboardEvent(
        source: *const c_void,
        virtual_key: u16,
        key_down: bool,
    ) -> *mut c_void;
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
    fn CGEventPost(tap: u32, event: *const c_void);
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: *const c_void);
}

const COPY_DELAY: Duration = Duration::from_millis(180);
const CLIPBOARD_POLL: Duration = Duration::from_millis(35);

pub fn capture_selected_text() -> Result<String> {
    thread::sleep(COPY_DELAY);
    let mut clipboard = Clipboard::new().context("시스템 클립보드를 열지 못했습니다")?;
    let previous = clipboard.get_text().ok();
    let marker = format!(
        "__DOG_TRANSLATE_COPY_MARKER_{}_{}__",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    clipboard
        .set_text(marker.clone())
        .context("선택문 복사를 위한 클립보드 준비에 실패했습니다")?;
    drop(clipboard);

    send_copy_shortcut().context("선택한 문장을 복사하지 못했습니다")?;
    for _ in 0..24 {
        thread::sleep(CLIPBOARD_POLL);
        if let Ok(text) = Clipboard::new().and_then(|mut clipboard| clipboard.get_text())
            && text != marker
            && !text.trim().is_empty()
        {
            return Ok(text.trim().to_owned());
        }
    }

    let mut clipboard = Clipboard::new().context("시스템 클립보드를 다시 열지 못했습니다")?;
    if let Some(previous) = previous {
        let _ = clipboard.set_text(previous);
    } else {
        let _ = clipboard.clear();
    }
    bail!("선택한 텍스트를 복사하지 못했습니다. 입력창에서 문장을 선택한 뒤 다시 시도하세요")
}

pub fn write_text(text: &str) -> Result<()> {
    Clipboard::new()
        .context("시스템 클립보드를 열지 못했습니다")?
        .set_text(text.to_owned())
        .context("번역문을 클립보드에 저장하지 못했습니다")
}

#[cfg(target_os = "windows")]
fn send_copy_shortcut() -> Result<()> {
    send_ctrl_key(VK_C)
}

#[cfg(target_os = "windows")]
fn send_ctrl_key(key: VIRTUAL_KEY) -> Result<()> {
    let inputs = [
        keyboard_input(VK_CONTROL, false),
        keyboard_input(key, false),
        keyboard_input(key, true),
        keyboard_input(VK_CONTROL, true),
    ];
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        bail!("Windows가 키 입력을 차단했습니다 ({sent}/{})", inputs.len());
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn keyboard_input(key: VIRTUAL_KEY, key_up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: 0,
                dwFlags: if key_up {
                    KEYEVENTF_KEYUP
                } else {
                    Default::default()
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(target_os = "macos")]
fn send_copy_shortcut() -> Result<()> {
    send_command_key(MAC_KEY_C)
}

#[cfg(target_os = "macos")]
fn send_command_key(key: u16) -> Result<()> {
    let down = unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), key, true) };
    let up = unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), key, false) };
    if down.is_null() || up.is_null() {
        if !down.is_null() {
            unsafe { CFRelease(down) };
        }
        if !up.is_null() {
            unsafe { CFRelease(up) };
        }
        bail!("macOS 키 입력 이벤트를 만들지 못했습니다");
    }

    unsafe {
        CGEventSetFlags(down, MAC_COMMAND_FLAG);
        CGEventSetFlags(up, MAC_COMMAND_FLAG);
        CGEventPost(MAC_HID_EVENT_TAP, down);
        CGEventPost(MAC_HID_EVENT_TAP, up);
        CFRelease(down);
        CFRelease(up);
    }
    Ok(())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn send_copy_shortcut() -> Result<()> {
    bail!("선택문 자동 복사는 현재 Windows와 macOS에서 지원합니다")
}
