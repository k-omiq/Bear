//! A copy of the app that stopped on its way out must not block the next launch.
//!
//! The single-instance plugin hands a second launch to the first copy with
//! `SendMessageW`, which has no timeout. A first copy that has lost its window
//! and stopped answering keeps its mutex and its hidden window, so the second
//! launch waits forever and shows nothing. This runs ahead of the plugin and ends
//! a copy in exactly that state. A copy with a window on screen is left alone,
//! because the person can see it and Windows offers to close it.

use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, HWND};
use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, GetWindowThreadProcessId, IsWindowVisible, SendMessageTimeoutW, SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_NULL,
};

/// How long a copy may leave a message unanswered. Long, because a copy hidden in
/// the tray may be running a job, and a launch that finds a healthy one never waits.
const ANSWER_TIMEOUT_MS: u32 = 10_000;
const EXIT_TIMEOUT_MS: u32 = 5_000;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Ends the copy holding the single-instance window of `identifier`, if it
/// answers nothing and shows nothing.
pub fn end(identifier: &str) {
    // The plugin's own names (tauri-plugin-single-instance, `platform_impl/windows.rs`).
    // Should they change, this finds no window and does nothing.
    let (class, title) = (wide(&format!("{identifier}-sic")), wide(&format!("{identifier}-siw")));
    let window = unsafe { FindWindowW(class.as_ptr(), title.as_ptr()) };
    if window.is_null() || answers(window, ANSWER_TIMEOUT_MS) {
        return;
    }
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(window, &mut pid) };
    if pid == 0 || shows_a_window(pid) {
        return;
    }
    eprintln!("manga-cleaner: ending copy {pid}, which has no window and does not answer");
    // Safety: a handle this function opened and closes, and no pointers kept.
    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid);
        if !process.is_null() {
            TerminateProcess(process, 1);
            WaitForSingleObject(process, EXIT_TIMEOUT_MS);
            CloseHandle(process);
        }
    }
}

/// Whether the thread that owns `window` handles a message within `timeout_ms`.
fn answers(window: HWND, timeout_ms: u32) -> bool {
    let mut result = 0;
    let sent = unsafe { SendMessageTimeoutW(window, WM_NULL, 0, 0, SMTO_ABORTIFHUNG | SMTO_BLOCK, timeout_ms, &mut result) };
    sent != 0
}

fn shows_a_window(pid: u32) -> bool {
    let mut window: HWND = null_mut();
    loop {
        window = unsafe { FindWindowExW(null_mut(), window, null(), null()) };
        if window.is_null() {
            return false;
        }
        let mut owner = 0;
        unsafe { GetWindowThreadProcessId(window, &mut owner) };
        if owner == pid && unsafe { IsWindowVisible(window) } != 0 {
            return true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;
    use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, DispatchMessageW, PeekMessageW, MSG, PM_REMOVE};

    /// A hidden window on its own thread, which pumps its messages or does not.
    /// The thread lives until `stop` is set.
    fn window_on_thread(pumps: bool, stop: Arc<AtomicBool>) -> (usize, std::thread::JoinHandle<()>) {
        let (sender, receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || unsafe {
            let class = wide("STATIC");
            let window = CreateWindowExW(0, class.as_ptr(), null(), 0, 0, 0, 0, 0, null_mut(), null_mut(), null_mut(), null());
            sender.send(window as usize).unwrap();
            let mut message: MSG = std::mem::zeroed();
            while !stop.load(Ordering::SeqCst) {
                while pumps && PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    DispatchMessageW(&message);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            DestroyWindow(window);
        });
        (receiver.recv().unwrap(), thread)
    }

    #[test]
    fn a_window_answers_only_while_its_thread_handles_messages() {
        let stop = Arc::new(AtomicBool::new(false));
        let (pumping, first) = window_on_thread(true, stop.clone());
        let (stopped, second) = window_on_thread(false, stop.clone());
        assert!(answers(pumping as HWND, 5_000));
        assert!(!answers(stopped as HWND, 200));
        stop.store(true, Ordering::SeqCst);
        first.join().unwrap();
        second.join().unwrap();
    }

    #[test]
    fn this_process_with_no_such_window_is_left_alone() {
        end("com.mangacleaner.test-no-such-copy");
    }
}
