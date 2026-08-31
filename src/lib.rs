mod logging;
mod ini;
mod monitor;

use std::{ffi::{OsString, c_void}, os::windows::ffi::OsStringExt, path::PathBuf, sync::{LazyLock, OnceLock}};

use anyhow::{anyhow, Result};
use ini::CONFIG;
use windows::Win32::{Foundation::{HMODULE, TRUE}, System::{LibraryLoader::{GetModuleFileNameW, LoadLibraryW}, SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH}}};
use windows_core::{BOOL, PCWSTR};

use crate::logging::message_box;

static DLL_PATH: OnceLock<PathBuf> = OnceLock::new();
static DLL: OnceLock<PathBuf> = OnceLock::new();

#[unsafe(no_mangle)]
pub extern "system" fn DllMain(module: HMODULE, reason: u32, _reserved: *const c_void) -> BOOL {
    match reason {
        DLL_PROCESS_ATTACH => {
            let dll_path = get_module_path(Some(module));
            DLL.set(dll_path.clone()).unwrap();
            DLL_PATH.set(dll_path.parent().unwrap().to_path_buf()).unwrap();

            let game_path = get_module_path(None);
            if let Some(exe) = game_path.file_name() {
                if exe.to_string_lossy().to_lowercase() == "rundll32.exe" {
                    return TRUE;
                }
            }

            logging::init_logger();
            logging::setup_panic_handler();

            initialize().unwrap_or_else(|e| {
                log::error!("Failed to initialize: {}", e);
                message_box(format!("Failed to initialize ColdLoader:\n{}", e).as_str());
                std::process::exit(1);
            });

            monitor::spawn_watchdog().unwrap_or_else(|e| {
                log::error!("Failed to spawn watchdog: {}", e);
            });
        }
        DLL_PROCESS_DETACH => {
            log::info!("DLL_PROCESS_DETACH called, performing cleanup");
            let _ = cleanup();
        }
        _ => {}
    }
    
    TRUE
}

static STEAM_UNIVERSE: &str = "public";
static PROCESS_ID: LazyLock<u32> = LazyLock::new(|| std::process::id());

const STEAMCLIENT_KEY: &str = if cfg!(target_pointer_width = "64") {
    "SteamClientDll64"
} else {
    "SteamClientDll"
};

fn patch_registry() -> Result<()> {
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .create_subkey("Software\\Valve\\Steam\\ActiveProcess")
        .map(|(key, _)| {
            let steamclient_path = CONFIG.steamclient_path
                .to_string_lossy()
                .to_string();
            
            let _ = key.set_value("pid", &*PROCESS_ID);
            let _ = key.set_value("Universe", &STEAM_UNIVERSE);
            let _ = key.set_value(STEAMCLIENT_KEY, &steamclient_path);
        })
        .map_err(|e| {
            log::error!("Unable to patch Registry (HKCU), error = {:?}", e);
            e
        })?;

    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .create_subkey("Software\\Valve\\Steam")
        .map(|(key, _)| {
            let steam_path = CONFIG.steamclient_path
                .parent()
                .unwrap()
                .to_string_lossy()
                .to_string();
            
            let _ = key.set_value("SteamPath", &steam_path);
            let _ = key.set_value("RunningAppID", &CONFIG.app_id);
        })
        .map_err(|e| {
            log::error!("Unable to patch Registry (HKCU #2), error = {:?}", e);
            e
        })?;

    Ok(())
}

fn set_steam_env_vars(app_id: u32) {
    unsafe {
        std::env::set_var("SteamAppId", app_id.to_string());
        std::env::set_var("SteamGameId", app_id.to_string());

        std::env::set_var("SteamAppUser", "cold_loader");
        std::env::set_var("SteamUser", "cold_loader");

        std::env::set_var("SteamClientLaunch", "1");
        std::env::set_var("SteamEnv", "1");
        std::env::set_var("SteamPath", std::env::current_exe().unwrap().parent().unwrap().to_string_lossy().to_string());
    }

    log::info!("Set Steam environment variables");
}

fn initialize() -> Result<()> {
    set_steam_env_vars(CONFIG.app_id);

    // Patch the registry
    patch_registry()?;
    
    // Load the DLLs
    let client_path = CONFIG.steamclient_path.to_string_lossy().to_string();
    log::info!("Loading steamclient from path: {}", client_path);
    let client_dll = unsafe {
        let path: Vec<u16> = client_path.encode_utf16().chain(std::iter::once(0)).collect();
        LoadLibraryW(PCWSTR(path.as_ptr()))
    };
    if client_dll.is_err() {
        return Err(anyhow!("Failed to load steamclient"));
    }

    log::info!("Loaded steamclient");

    let overlay_dll_name = if cfg!(target_pointer_width = "64") {
        "gameoverlayrenderer64.dll"
    } else {
        "gameoverlayrenderer.dll"
    };

    let gameoverlay_path = std::env::current_exe().unwrap().parent().unwrap().join(overlay_dll_name);
    if gameoverlay_path.exists() {
        let gameoverlay_dll = unsafe {
            let path: Vec<u16> = gameoverlay_path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
            LoadLibraryW(PCWSTR(path.as_ptr()))
        };
        if gameoverlay_dll.is_err() {
            return Err(anyhow!("Failed to load {}", overlay_dll_name));
        }

        log::info!("Loaded {}", overlay_dll_name);
    } else {
        log::warn!("{} not found, skipping load", overlay_dll_name);
    }

    Ok(())
}

pub fn cleanup() -> Result<()> {
    let install_path = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags("Software\\WOW6432Node\\Valve\\Steam", winreg::enums::KEY_ALL_ACCESS)
        .and_then(|key| {
            let path = key.get_value::<String, _>("InstallPath")?;
            Ok(PathBuf::from(path))
        });

    if let Ok(path) = install_path {
        let _ = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
            .open_subkey_with_flags("Software\\Valve\\Steam\\ActiveProcess", winreg::enums::KEY_ALL_ACCESS)
            .map(|key| {
                let steamclient_dll = if cfg!(target_pointer_width = "64") {
                    path.join("steamclient64.dll")
                } else {
                    path.join("steamclient.dll")
                };

                let _ = key.set_value(STEAMCLIENT_KEY, &steamclient_dll.to_string_lossy().to_string());
            });
        
        let _ = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
            .open_subkey_with_flags("Software\\Valve\\Steam", winreg::enums::KEY_ALL_ACCESS)
            .map(|key| {
                let _ = key.set_value("SteamPath", &path.to_string_lossy().to_string());
            });
    }

    log::info!("Cleaned up registry");
    Ok(())
}

fn get_module_path(module: Option<HMODULE>) -> PathBuf {
    let mut buffer = [0u16; 1024];
    let len = unsafe { GetModuleFileNameW(module, &mut buffer) };

    let path = PathBuf::from(OsString::from_wide(&buffer[..len as usize]));
    path
}
