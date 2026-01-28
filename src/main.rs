//! Push-to-talk mic control for PipeWire using evdev input devices.

mod audio;
mod config;
mod constants;

use anyhow::{bail, Context, Result};
use evdev::{Device, EventSummary, KeyCode};
use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use crate::audio::{apply_off, apply_on, init_audio_cache, play_transition_sound};
use crate::config::{
    backup_config_path, config_from_persisted, config_path, load_persisted_config, parse_args,
    persisted_from_config, print_config, print_persisted_config, print_supported_keys,
    read_persisted_config, restart_service, write_persisted_config, Config, StartupState,
};

fn print_devices() -> Result<()> {
    for (path, device) in evdev::enumerate() {
        let name = device.name().unwrap_or("unknown");
        println!("{} - {}", path.display(), name);
    }
    Ok(())
}

/// Open the input device, using an explicit path or by probing available devices.
fn open_device(config: &Config) -> Result<Device> {
    if let Some(path) = &config.device_path {
        let device = Device::open(path)
            .with_context(|| format!("Failed to open device {}", path.display()))?;
        if let Some(keys) = device.supported_keys() {
            for key in &config.keys {
                if !keys.contains(*key) {
                    bail!(
                        "Device {} does not support key {}",
                        path.display(),
                        key.code()
                    );
                }
            }
        }
        return Ok(device);
    }

    let mut devices: Vec<Device> = evdev::enumerate()
        .map(|(_, d)| d)
        .filter(|d| {
            d.supported_keys()
                .map(|k| config.keys.iter().all(|key| k.contains(*key)))
                .unwrap_or(false)
        })
        .collect();

    if devices.is_empty() {
        bail!("No input device found that supports all configured keys");
    }

    Ok(devices.remove(0))
}

fn apply_startup_state(config: &Config) -> Result<()> {
    match config.startup_state {
        StartupState::Muted => apply_off(config),
        StartupState::Unmuted => apply_on(config),
    }
}

fn set_active_state(config: &Config, active: &mut bool, on: bool) -> Result<()> {
    if on {
        apply_on(config)?;
        play_transition_sound(config, true);
        println!("🎤 ON");
    } else {
        apply_off(config)?;
        play_transition_sound(config, false);
        println!("🔇 OFF");
    }
    *active = on;
    Ok(())
}

fn update_pressed_keys(pressed: &mut HashSet<KeyCode>, key: KeyCode, value: i32) {
    match value {
        1 => {
            pressed.insert(key);
        }
        0 => {
            pressed.remove(&key);
        }
        _ => {}
    }
}

fn refresh_active_state(
    config: &Config,
    pressed: &HashSet<KeyCode>,
    active: &mut bool,
) -> Result<()> {
    let all_pressed = config.keys.iter().all(|k| pressed.contains(k));
    let desired_on = if config.reverse { !all_pressed } else { all_pressed };
    if desired_on != *active {
        set_active_state(config, active, desired_on)?;
    }
    Ok(())
}

fn handle_events(
    config: &Config,
    device: &mut Device,
    pressed: &mut HashSet<KeyCode>,
    active: &mut bool,
) -> Result<Option<std::io::Error>> {
    let fetch_error = match device.fetch_events() {
        Ok(events) => {
            for ev in events {
                if let EventSummary::Key(_, key, value) = ev.destructure() {
                    update_pressed_keys(pressed, key, value);
                    refresh_active_state(config, pressed, active)?;
                }
            }
            None
        }
        Err(err) => {
            if err.kind() == ErrorKind::WouldBlock {
                None
            } else {
                Some(err)
            }
        }
    };
    Ok(fetch_error)
}

fn is_permission_denied(err: &anyhow::Error) -> bool {
    err.to_string().contains("Permission denied")
}

fn open_device_with_hint(config: &Config) -> Result<Device> {
    match open_device(config) {
        Ok(device) => Ok(device),
        Err(err) => {
            if is_permission_denied(&err) {
                eprintln!("Hint: add your user to the input group or add a udev rule.");
            }
            Err(err)
        }
    }
}

fn reopen_device_loop(config: &Config) -> Result<Device> {
    loop {
        match open_device_nonblocking(config) {
            Ok(reopened) => return Ok(reopened),
            Err(open_err) => {
                if is_permission_denied(&open_err) {
                    return Err(open_err);
                }
                eprintln!("Retrying device open: {open_err}");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn set_device_nonblocking(device: &Device) -> Result<()> {
    let fd = device.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        bail!(
            "Failed to read device flags: {}",
            std::io::Error::last_os_error()
        );
    }
    let res = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if res < 0 {
        bail!(
            "Failed to set device non-blocking: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

fn open_device_nonblocking(config: &Config) -> Result<Device> {
    let device = open_device_with_hint(config)?;
    set_device_nonblocking(&device)?;
    Ok(device)
}

fn config_mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

fn spawn_config_watcher(
    config_path: std::path::PathBuf,
    running: Arc<AtomicBool>,
) -> mpsc::Receiver<Config> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut last_modified = config_mtime(&config_path);
        while running.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(500));
            let modified = config_mtime(&config_path);
            if modified != last_modified {
                last_modified = modified;
                if modified.is_none() {
                    continue;
                }
                match read_persisted_config(&config_path).and_then(config_from_persisted) {
                    Ok(config) => {
                        let _ = tx.send(config);
                    }
                    Err(err) => {
                        eprintln!("Failed to reload config: {err}");
                    }
                }
            }
        }
    });
    rx
}

fn main() -> Result<()> {
    let (base_config, created, config_path_used) = load_persisted_config()?;
    print_persisted_config(&config_path_used, &base_config);
    let (mut config, persist_changed) = parse_args(base_config)?;
    if persist_changed {
        let persisted = persisted_from_config(&config);
        let primary = config_path()?;
        let backup = backup_config_path()?;
        write_persisted_config(&persisted, &primary, &backup)?;
        restart_service();
        return Ok(());
    }
    if created {
        let persisted = persisted_from_config(&config);
        let primary = config_path()?;
        let backup = backup_config_path()?;
        write_persisted_config(&persisted, &primary, &backup)?;
    }

    if config.list_keys {
        print_supported_keys();
        return Ok(());
    }
    if config.list_devices {
        return print_devices();
    }
    if config.print_config {
        print_config(&config);
        if config.dry_run {
            let _ = open_device(&config)?;
        }
        return Ok(());
    }

    if config.dry_run {
        let _ = open_device(&config)?;
        println!("Dry run OK");
        return Ok(());
    }

    init_audio_cache(&config)?;

    // Ensure mic is muted immediately on start
    apply_startup_state(&config)?;
    match config.startup_state {
        StartupState::Muted => println!("🔇 Mic muted on start"),
        StartupState::Unmuted => println!("🎤 Mic unmuted on start"),
    }

    // Ensure mic is muted on exit / crash
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })
    .expect("Failed to set Ctrl-C handler");

    let config_updates = spawn_config_watcher(config_path_used, running.clone());
    let mut device = open_device_nonblocking(&config)?;

    if config.reverse {
        println!("🎙 Hold the configured button to mute");
    } else {
        println!("🎙 Hold the configured button to talk");
    }

    let mut pressed: HashSet<KeyCode> = HashSet::new();
    let mut active = false;

    refresh_active_state(&config, &pressed, &mut active)?;

    while running.load(Ordering::SeqCst) {
        if let Some(err) = handle_events(&config, &mut device, &mut pressed, &mut active)? {
            eprintln!("Input device error: {err}. Reopening...");
            apply_off(&config)?;
            active = false;
            pressed.clear();
            device = reopen_device_loop(&config)?;
        }

        if let Ok(new_config) = config_updates.try_recv() {
            let keys_changed = config.keys != new_config.keys;
            let device_changed = config.device_path != new_config.device_path;
            config = new_config;
            if let Err(err) = init_audio_cache(&config) {
                eprintln!("Failed to reload sounds: {err}");
            }
            if keys_changed || device_changed {
                apply_off(&config)?;
                active = false;
                pressed.clear();
                device = reopen_device_loop(&config)?;
            }
            refresh_active_state(&config, &pressed, &mut active)?;
            println!("Config reloaded");
        }

        std::thread::sleep(Duration::from_millis(10));
    }

    // Final safety mute
    apply_off(&config)?;
    println!("🔇 Mic muted on exit");

    Ok(())
}
