use windows::{Win32::{Foundation::{CloseHandle, HINSTANCE, HWND}, System::Threading::{CREATE_NO_WINDOW, CreateProcessW, INFINITE, OpenProcess, PROCESS_INFORMATION, PROCESS_SYNCHRONIZE, STARTF_USESHOWWINDOW, STARTUPINFOW, WaitForSingleObject}, UI::WindowsAndMessaging::SW_HIDE}, core::HSTRING};
use windows_core::{PCSTR, PWSTR};

use crate::{DLL, cleanup};

#[unsafe(no_mangle)]
pub extern "system" fn watchdog(
    _hwnd: HWND,
    _hinst: HINSTANCE,
    cmd_line: PCSTR,
    _cmd_show: i32
) {
    let cmd_line_str = unsafe { std::ffi::CStr::from_ptr(cmd_line.0 as *const _) }.to_str().unwrap_or_default();
    let parts = cmd_line_str.split_whitespace().collect::<Vec<_>>();
    let pid = parts.last().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);

    let _ = init_watchdog(pid);
}

fn init_watchdog(pid: u32) -> Result<(), String> {
    if pid == 0 {
        return Err("Invalid PID".to_string());
    }

    unsafe {
        let h_process = OpenProcess(PROCESS_SYNCHRONIZE, false, pid)
            .map_err(|e| format!("Failed to open process: {:?}", e))?;

        WaitForSingleObject(h_process, INFINITE);
        let _ = CloseHandle(h_process);
    }

    cleanup().map_err(|e| format!("Cleanup failed: {:?}", e))?;

    Ok(())
}

pub fn spawn_watchdog() -> Result<(), String> {
    let pid = std::process::id();

    let dll_path = DLL.get().unwrap().to_string_lossy();
    let cmd_str = format!(
        r#"rundll32.exe "{}",watchdog {}"#,
        dll_path,
        pid
    );
    let wide_cmd_str = HSTRING::from(cmd_str);

    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    si.dwFlags = STARTF_USESHOWWINDOW;
    si.wShowWindow = SW_HIDE.0 as u16;

    let result = unsafe {
        CreateProcessW(
            None,
            Some(PWSTR(wide_cmd_str.as_ptr() as *mut _)),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            None,
            &mut si,
            &mut pi
        )
    };

    match result {
        Ok(_) => unsafe {
            let _ = CloseHandle(pi.hProcess);
            let _ = CloseHandle(pi.hThread);
        }
        Err(e) => {
            return Err(format!("Failed to spawn watchdog process: {:?}", e));
        }
    }

    log::info!("Watchdog process spawned successfully with PID: {}", pi.dwProcessId);
    
    Ok(())
}
