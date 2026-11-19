//! The few Windows functions zone calls itself: values in the person's own part of the registry
//! (the login entry), the clipboard, and keeping a program started in the background from
//! holding this one's output open. Everything else goes through the standard library.
//!
//! Both are done here rather than by starting `reg.exe` or `clip.exe`: those read and write
//! text in the console's code page, so a name like `C:\Users\Zoë` or a clip in Japanese would
//! not arrive as it was, and starting a program for each is slower.

use std::io;
use std::ptr::{null, null_mut};
use std::time::Duration;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, GetHandleInformation, GlobalFree, HANDLE_FLAG_INHERIT, HWND, SetHandleInformation};
use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_NOEXPAND, RRF_RT_REG_BINARY, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE};

/// Text as Windows wants it: UTF-16, ending in a zero.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn error(code: u32) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

/// The raw bytes of a value under `HKEY_CURRENT_USER`, when it is there and of one of `kinds`.
fn get_value(subkey: &str, name: &str, kinds: u32) -> Option<Vec<u8>> {
    let (subkey, name) = (wide(subkey), wide(name));
    // The value can change between asking its size and reading it, so this is tried twice.
    for _ in 0..2 {
        let mut size: u32 = 0;
        // SAFETY: the strings end in a zero; asking the size only, so no buffer is written.
        let asked = unsafe { RegGetValueW(HKEY_CURRENT_USER, subkey.as_ptr(), name.as_ptr(), kinds, null_mut(), null_mut(), &mut size) };
        if asked != ERROR_SUCCESS {
            return None;
        }
        let mut bytes = vec![0u8; size as usize];
        // SAFETY: `bytes` holds `size` bytes, and the call writes at most that many.
        let read = unsafe { RegGetValueW(HKEY_CURRENT_USER, subkey.as_ptr(), name.as_ptr(), kinds, null_mut(), bytes.as_mut_ptr().cast(), &mut size) };
        if read == ERROR_SUCCESS {
            bytes.truncate(size as usize);
            return Some(bytes);
        }
    }
    None
}

/// A text value under `HKEY_CURRENT_USER`, as it is stored (nothing in it expanded).
pub fn reg_get_string(subkey: &str, name: &str) -> Option<String> {
    let bytes = get_value(subkey, name, RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND)?;
    let units: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
    let end = units.iter().position(|unit| *unit == 0).unwrap_or(units.len());
    Some(String::from_utf16_lossy(&units[..end]))
}

/// A binary value under `HKEY_CURRENT_USER`.
pub fn reg_get_binary(subkey: &str, name: &str) -> Option<Vec<u8>> {
    get_value(subkey, name, RRF_RT_REG_BINARY)
}

/// Writes a text value under `HKEY_CURRENT_USER`, making the key when it is missing.
pub fn reg_set_string(subkey: &str, name: &str, value: &str) -> io::Result<()> {
    let (subkey, name, data) = (wide(subkey), wide(name), wide(value));
    // SAFETY: every string ends in a zero, and the size given is the data's, zero included.
    let done = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, subkey.as_ptr(), name.as_ptr(), REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32) };
    if done == ERROR_SUCCESS { Ok(()) } else { Err(error(done)) }
}

/// Removes a value under `HKEY_CURRENT_USER`. False when it was not there.
pub fn reg_delete(subkey: &str, name: &str) -> io::Result<bool> {
    let (subkey, name) = (wide(subkey), wide(name));
    // SAFETY: both strings end in a zero.
    let done = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, subkey.as_ptr(), name.as_ptr()) };
    match done {
        ERROR_SUCCESS => Ok(true),
        ERROR_FILE_NOT_FOUND => Ok(false),
        other => Err(error(other)),
    }
}

/// Runs `start` (which starts a program that outlives this one) while this program's standard
/// input, output and error cannot be handed down.
///
/// Windows hands every handle that may be inherited to a program it starts, whatever that
/// program's own input and output are. A zone started in the background would then hold the
/// output of the `zone start` that started it, and whatever reads that output (a script, an SSH
/// session) would wait for it to close for as long as zone runs.
pub fn without_handing_down_stdio<T>(start: impl FnOnce() -> T) -> T {
    use std::os::windows::io::AsRawHandle;
    let handles = [std::io::stdin().as_raw_handle(), std::io::stdout().as_raw_handle(), std::io::stderr().as_raw_handle()];
    let mut restore = Vec::new();
    for handle in handles {
        if handle.is_null() || restore.contains(&handle) {
            continue;
        }
        let mut flags = 0u32;
        // SAFETY: this process's own standard handles; only their inherit flag is changed.
        if unsafe { GetHandleInformation(handle, &mut flags) } != 0 && flags & HANDLE_FLAG_INHERIT != 0 && unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } != 0 {
            restore.push(handle);
        }
    }
    let result = start();
    for handle in restore {
        // SAFETY: as above, setting back what was there.
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    }
    result
}

/// Puts text on the clipboard, as Unicode text, the way a program does when the person copies.
///
/// The clipboard is shared, and another program may hold it for a moment (a clipboard manager
/// reading what was just copied), so opening it is tried for about a second.
pub fn set_clipboard_text(text: &str) -> io::Result<()> {
    let units = wide(text);
    let class = wide("STATIC");
    // A window of zone's own, never shown, to own what it puts on the clipboard: with no owner
    // some versions of Windows refuse the text.
    // SAFETY: a message-only window of a class the system always has; it is destroyed below.
    let owner: HWND = unsafe { CreateWindowExW(0, class.as_ptr(), null(), 0, 0, 0, 0, 0, HWND_MESSAGE, null_mut(), null_mut(), null()) };
    let result = with_open_clipboard(owner, || {
        // SAFETY: the clipboard is open, by this thread.
        if unsafe { EmptyClipboard() } == 0 {
            return Err(io::Error::last_os_error());
        }
        let bytes = units.len() * 2;
        // SAFETY: a plain allocation, freed below unless the clipboard takes it.
        let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) };
        if memory.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `memory` is a movable block of `bytes` bytes. Locked, it is written with
        // exactly that many, then unlocked before the clipboard is given it.
        unsafe {
            let target = GlobalLock(memory) as *mut u16;
            if target.is_null() {
                let error = io::Error::last_os_error();
                GlobalFree(memory);
                return Err(error);
            }
            std::ptr::copy_nonoverlapping(units.as_ptr(), target, units.len());
            GlobalUnlock(memory);
            if SetClipboardData(CF_UNICODETEXT as u32, memory).is_null() {
                let error = io::Error::last_os_error();
                GlobalFree(memory);
                return Err(error);
            }
        }
        // The clipboard owns the memory now.
        Ok(())
    });
    if !owner.is_null() {
        // SAFETY: the window made above, by this thread.
        unsafe { DestroyWindow(owner) };
    }
    result
}

fn with_open_clipboard(owner: HWND, work: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    let mut last = None;
    for attempt in 0..40 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(25));
        }
        // SAFETY: `owner` is this thread's window, or null.
        if unsafe { OpenClipboard(owner) } != 0 {
            let result = work();
            // SAFETY: opened just above, by this thread.
            unsafe { CloseClipboard() };
            return result;
        }
        last = Some(io::Error::last_os_error());
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, format!("another program kept the clipboard open ({})", last.map(|e| e.to_string()).unwrap_or_default())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_values_are_written_read_and_removed() {
        // A key of its own, so nothing real is touched; removed again at the end.
        let key = format!(r"Software\zone-test-{}", std::process::id());
        let text = "C:\\Users\\Zo\u{eb}\\\u{65e5}\u{672c}\\zonew.exe\" serve --background";
        reg_set_string(&key, "Zone", text).unwrap();
        assert_eq!(reg_get_string(&key, "Zone").as_deref(), Some(text));
        assert_eq!(reg_get_binary(&key, "Zone"), None, "a text value is not binary");
        assert!(reg_delete(&key, "Zone").unwrap());
        assert!(!reg_delete(&key, "Zone").unwrap());
        assert_eq!(reg_get_string(&key, "Zone"), None);
        let wide_key = wide(&key);
        // SAFETY: a key made by this test, named with a terminating zero.
        unsafe { windows_sys::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, wide_key.as_ptr()) };
    }
}
