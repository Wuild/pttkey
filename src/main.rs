//! Push-to-talk mic control for PipeWire using evdev input devices.

use anyhow::{bail, Context, Result};
use evdev::{Device, EventSummary, KeyCode};
use std::collections::HashSet;
use std::env;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_SOUND_ON_EVENT: &str = "audio-volume-change";
const DEFAULT_SOUND_OFF_EVENT: &str = "audio-volume-muted";

macro_rules! key_map {
    ($($key:ident),+ $(,)?) => {
        &[
            $(
                (stringify!($key), KeyCode::$key)
            ),+
        ]
    };
}

const SUPPORTED_KEYS: &[(&str, KeyCode)] = key_map![
    // Mouse buttons,
    BTN_LEFT,
    BTN_RIGHT,
    BTN_MIDDLE,
    BTN_SIDE,
    BTN_EXTRA,
    BTN_FORWARD,
    BTN_BACK,
    // Letters,
    KEY_A,
    KEY_B,
    KEY_C,
    KEY_D,
    KEY_E,
    KEY_F,
    KEY_G,
    KEY_H,
    KEY_I,
    KEY_J,
    KEY_K,
    KEY_L,
    KEY_M,
    KEY_N,
    KEY_O,
    KEY_P,
    KEY_Q,
    KEY_R,
    KEY_S,
    KEY_T,
    KEY_U,
    KEY_V,
    KEY_W,
    KEY_X,
    KEY_Y,
    KEY_Z,
    // Numbers row,
    KEY_0,
    KEY_1,
    KEY_2,
    KEY_3,
    KEY_4,
    KEY_5,
    KEY_6,
    KEY_7,
    KEY_8,
    KEY_9,
    // Function keys,
    KEY_F1,
    KEY_F2,
    KEY_F3,
    KEY_F4,
    KEY_F5,
    KEY_F6,
    KEY_F7,
    KEY_F8,
    KEY_F9,
    KEY_F10,
    KEY_F11,
    KEY_F12,
    // Modifiers and whitespace,
    KEY_LEFTCTRL,
    KEY_RIGHTCTRL,
    KEY_LEFTSHIFT,
    KEY_RIGHTSHIFT,
    KEY_LEFTALT,
    KEY_RIGHTALT,
    KEY_LEFTMETA,
    KEY_RIGHTMETA,
    KEY_CAPSLOCK,
    KEY_TAB,
    KEY_SPACE,
    KEY_ENTER,
    KEY_ESC,
    KEY_BACKSPACE,
    // Navigation,
    KEY_UP,
    KEY_DOWN,
    KEY_LEFT,
    KEY_RIGHT,
    KEY_HOME,
    KEY_END,
    KEY_PAGEUP,
    KEY_PAGEDOWN,
    KEY_INSERT,
    KEY_DELETE,
    // Punctuation,
    KEY_MINUS,
    KEY_EQUAL,
    KEY_LEFTBRACE,
    KEY_RIGHTBRACE,
    KEY_BACKSLASH,
    KEY_SEMICOLON,
    KEY_APOSTROPHE,
    KEY_GRAVE,
    KEY_COMMA,
    KEY_DOT,
    KEY_SLASH,
    // Numpad,
    KEY_NUMLOCK,
    KEY_KPSLASH,
    KEY_KPASTERISK,
    KEY_KPMINUS,
    KEY_KPPLUS,
    KEY_KPENTER,
    KEY_KP0,
    KEY_KP1,
    KEY_KP2,
    KEY_KP3,
    KEY_KP4,
    KEY_KP5,
    KEY_KP6,
    KEY_KP7,
    KEY_KP8,
    KEY_KP9,
    KEY_KPDOT,
    // Media,
    KEY_MUTE,
    KEY_VOLUMEDOWN,
    KEY_VOLUMEUP,
    KEY_PLAYPAUSE,
    KEY_NEXTSONG,
    KEY_PREVIOUSSONG,
    KEY_STOPCD,
];

/// How the mic is toggled: by absolute volume level or by mute state.
#[derive(Copy, Clone, Debug)]
enum Mode {
    Volume,
    Mute,
}

/// Startup behavior for setting the mic state at launch.
#[derive(Copy, Clone, Debug)]
enum StartupState {
    Muted,
    Unmuted,
}

/// Runtime configuration assembled from CLI arguments.
#[derive(Clone, Debug)]
struct Config {
    /// Keys that must be held simultaneously to activate the mic.
    keys: Vec<KeyCode>,
    /// Optional explicit input device path (e.g. /dev/input/event7).
    device_path: Option<PathBuf>,
    /// Volume vs mute behavior.
    mode: Mode,
    /// Volume level when active.
    on_level: f32,
    /// Volume level when inactive.
    off_level: f32,
    /// Enable or disable sound effects.
    sounds: bool,
    /// Optional custom sound file for mic on.
    sound_on: Option<PathBuf>,
    /// Optional custom sound file for mic off.
    sound_off: Option<PathBuf>,
    /// Print available keys and exit.
    list_keys: bool,
    /// Print available input devices and exit.
    list_devices: bool,
    /// Print configuration and exit.
    print_config: bool,
    /// Validate inputs and exit without changing mic state.
    dry_run: bool,
    /// Startup mic state.
    startup_state: StartupState,
}

/// Set the default microphone volume to an absolute level.
fn set_volume(level: f32) -> Result<()> {
    Command::new("wpctl")
        .args([
            "set-volume",
            "@DEFAULT_SOURCE@",
            &format!("{level}"),
        ])
        .status()
        .context("wpctl failed")?;
    Ok(())
}

/// Mute or unmute the default microphone source.
fn set_mute(muted: bool) -> Result<()> {
    Command::new("wpctl")
        .args([
            "set-mute",
            "@DEFAULT_SOURCE@",
            if muted { "1" } else { "0" },
        ])
        .status()
        .context("wpctl failed")?;
    Ok(())
}

/// Play a user-supplied audio file (mp3/wav/ogg). Best-effort, async.
fn play_sound_file(path: PathBuf) {
    // Best-effort: play in a background thread to avoid blocking input handling.
    std::thread::spawn(move || {
        if let Ok(mut stream) = OutputStreamBuilder::open_default_stream() {
            stream.log_on_drop(false);
            if let Ok(file) = File::open(&path) {
                if let Ok(decoder) = Decoder::new(BufReader::new(file)) {
                    let sink = Sink::connect_new(stream.mixer());
                    sink.append(decoder);
                    sink.sleep_until_end();
                }
            }
        }
    });
}

fn find_bin(name: &str) -> Option<PathBuf> {
    if let Ok(path) = which::which(name) {
        return Some(path);
    }
    let fallback = PathBuf::from(format!("/usr/bin/{name}"));
    if fallback.exists() {
        return Some(fallback);
    }
    None
}

fn try_paplay(on: bool) -> bool {
    let Some(path) = find_bin("paplay") else {
        return false;
    };
    let candidates = if on {
        [
            "/usr/share/sounds/freedesktop/stereo/audio-volume-change.oga",
            "/usr/share/sounds/freedesktop/stereo/audio-volume-change.wav",
            "/usr/share/sounds/freedesktop/stereo/audio-volume-change.ogg",
        ]
    } else {
        [
            "/usr/share/sounds/freedesktop/stereo/audio-volume-muted.oga",
            "/usr/share/sounds/freedesktop/stereo/audio-volume-muted.wav",
            "/usr/share/sounds/freedesktop/stereo/audio-volume-muted.ogg",
        ]
    };
    for candidate in candidates {
        if Path::new(candidate).exists() {
            if let Ok(status) = Command::new(&path).arg(candidate).status() {
                return status.success();
            }
        }
    }
    false
}

fn try_canberra(event: &str) -> bool {
    let Some(path) = find_bin("canberra-gtk-play") else {
        return false;
    };
    if let Ok(status) = Command::new(path).args(["-i", event]).status() {
        return status.success();
    }
    false
}

/// Play the system default sound effect for on/off (best-effort, async).
fn play_default_sound(on: bool) {
    let event = if on {
        DEFAULT_SOUND_ON_EVENT
    } else {
        DEFAULT_SOUND_OFF_EVENT
    };
    std::thread::spawn(move || {
        if try_paplay(on) {
            return;
        }
        let _ = try_canberra(event);
    });
}

/// Apply the "mic on" action according to the selected mode.
fn apply_on(config: &Config) -> Result<()> {
    match config.mode {
        Mode::Volume => set_volume(config.on_level),
        Mode::Mute => set_mute(false),
    }
}

/// Apply the "mic off" action according to the selected mode.
fn apply_off(config: &Config) -> Result<()> {
    match config.mode {
        Mode::Volume => set_volume(config.off_level),
        Mode::Mute => set_mute(true),
    }
}

/// Parse a single key identifier (name or numeric evdev code) into a KeyCode.
fn parse_key(input: &str) -> Result<KeyCode> {
    let normalized = input.trim().to_ascii_uppercase();
    if let Ok(code) = normalized.parse::<u16>() {
        return Ok(KeyCode::new(code));
    }

    for (name, key) in SUPPORTED_KEYS {
        if *name == normalized {
            return Ok(*key);
        }
    }

    bail!(
        "Unknown key '{input}'. Use a numeric key code or a known name like BTN_EXTRA/KEY_F9."
    )
}

/// Parse a + separated key chord (e.g. KEY_LEFTCTRL+KEY_F).
fn parse_keys(input: &str) -> Result<Vec<KeyCode>> {
    input
        .split('+')
        .map(|part| parse_key(part.trim()))
        .collect()
}

/// Print CLI usage and examples.
fn print_help() {
    println!(
        "pttkey\n\
Usage: pttkey [options]\n\
\n\
Options:\n\
  --key <NAME|CODE>   evdev key name or numeric code; can repeat or use '+'\n\
                      (e.g. --key KEY_LEFTCTRL+KEY_F or --key KEY_LEFTCTRL --key KEY_F)\n\
  --device <PATH>     use a specific input device (e.g. /dev/input/event7)\n\
  --mode <volume|mute>  toggle by volume level or set-mute (default: volume)\n\
  --on-level <FLOAT>  volume level when pressed (default: 1.0)\n\
  --off-level <FLOAT> volume level when released (default: 0.0)\n\
  --sound-on <PATH>   custom sound file for mic on (mp3/wav/ogg)\n\
  --sound-off <PATH>  custom sound file for mic off (mp3/wav/ogg)\n\
  --startup-state <muted|unmuted>  initial mic state (default: muted)\n\
  --sounds            enable on/off sounds (default)\n\
  --no-sounds         disable on/off sounds\n\
  --list-keys         print supported key names and exit\n\
  --list-devices      print input devices and exit\n\
  --print-config      print parsed configuration and exit\n\
  --dry-run           validate configuration and exit without changing mic state\n\
  -h, --help          show this help\n\
\n\
Examples:\n\
  pttkey --key BTN_EXTRA\n\
  pttkey --key KEY_F9 --mode mute --no-sounds\n\
  pttkey --key KEY_LEFTCTRL+KEY_F --mode mute\n\
  pttkey --sound-on ~/on.wav --sound-off ~/off.ogg\n\
  pttkey --device /dev/input/event7 --key KEY_SPACE\n"
    );
}

fn print_supported_keys() {
    for (name, _) in SUPPORTED_KEYS {
        println!("{name}");
    }
}

fn print_devices() -> Result<()> {
    for (path, device) in evdev::enumerate() {
        let name = device.name().unwrap_or("unknown");
        println!("{} - {}", path.display(), name);
    }
    Ok(())
}

fn key_label(key: KeyCode) -> String {
    for (name, k) in SUPPORTED_KEYS {
        if *k == key {
            return (*name).to_string();
        }
    }
    format!("{}", key.code())
}

fn print_config(config: &Config) {
    let keys = config
        .keys
        .iter()
        .map(|k| key_label(*k))
        .collect::<Vec<_>>()
        .join("+");
    let device = config
        .device_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "auto".to_string());
    let mode = match config.mode {
        Mode::Volume => "volume",
        Mode::Mute => "mute",
    };
    let startup_state = match config.startup_state {
        StartupState::Muted => "muted",
        StartupState::Unmuted => "unmuted",
    };
    println!("keys: {keys}");
    println!("device: {device}");
    println!("mode: {mode}");
    println!("on_level: {}", config.on_level);
    println!("off_level: {}", config.off_level);
    println!("sounds: {}", config.sounds);
    println!(
        "sound_on: {}",
        config
            .sound_on
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "default".to_string())
    );
    println!(
        "sound_off: {}",
        config
            .sound_off
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "default".to_string())
    );
    println!("startup_state: {startup_state}");
}

/// Parse CLI arguments into runtime configuration.
fn parse_args() -> Result<Config> {
    let mut keys: Vec<KeyCode> = vec![KeyCode::BTN_EXTRA];
    let mut device_path = None;
    let mut mode = Mode::Volume;
    let mut on_level = 1.0;
    let mut off_level = 0.0;
    let mut sounds = true;
    let mut sound_on = None;
    let mut sound_off = None;
    let mut list_keys = false;
    let mut list_devices = false;
    let mut print_config = false;
    let mut dry_run = false;
    let mut startup_state = StartupState::Muted;

    let args: Vec<String> = env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "--key" => {
                i += 1;
                let value = args.get(i).context("missing value for --key")?;
                let mut parsed = parse_keys(value)?;
                if keys.len() == 1 && keys[0] == KeyCode::BTN_EXTRA {
                    keys.clear();
                }
                keys.append(&mut parsed);
            }
            "--device" => {
                i += 1;
                let value = args.get(i).context("missing value for --device")?;
                device_path = Some(PathBuf::from(value));
            }
            "--mode" => {
                i += 1;
                let value = args.get(i).context("missing value for --mode")?;
                mode = match value.as_str() {
                    "volume" => Mode::Volume,
                    "mute" => Mode::Mute,
                    _ => bail!("Invalid --mode '{value}'. Use 'volume' or 'mute'."),
                };
            }
            "--on-level" => {
                i += 1;
                let value = args.get(i).context("missing value for --on-level")?;
                on_level = value
                    .parse::<f32>()
                    .with_context(|| format!("invalid --on-level '{value}'"))?;
            }
            "--off-level" => {
                i += 1;
                let value = args.get(i).context("missing value for --off-level")?;
                off_level = value
                    .parse::<f32>()
                    .with_context(|| format!("invalid --off-level '{value}'"))?;
            }
            "--sound-on" => {
                i += 1;
                let value = args.get(i).context("missing value for --sound-on")?;
                sound_on = Some(PathBuf::from(value));
            }
            "--sound-off" => {
                i += 1;
                let value = args.get(i).context("missing value for --sound-off")?;
                sound_off = Some(PathBuf::from(value));
            }
            "--startup-state" => {
                i += 1;
                let value = args.get(i).context("missing value for --startup-state")?;
                startup_state = match value.as_str() {
                    "muted" => StartupState::Muted,
                    "unmuted" => StartupState::Unmuted,
                    _ => bail!(
                        "Invalid --startup-state '{value}'. Use 'muted' or 'unmuted'."
                    ),
                };
            }
            "--sounds" => {
                sounds = true;
            }
            "--no-sounds" => {
                sounds = false;
            }
            "--list-keys" => {
                list_keys = true;
            }
            "--list-devices" => {
                list_devices = true;
            }
            "--print-config" => {
                print_config = true;
            }
            "--dry-run" => {
                dry_run = true;
            }
            other => bail!("Unknown argument '{other}'. Use --help."),
        }
        i += 1;
    }

    if let Some(path) = &sound_on {
        if !path.exists() {
            bail!("Sound on file does not exist: {}", path.display());
        }
    }
    if let Some(path) = &sound_off {
        if !path.exists() {
            bail!("Sound off file does not exist: {}", path.display());
        }
    }

    Ok(Config {
        keys,
        device_path,
        mode,
        on_level,
        off_level,
        sounds,
        sound_on,
        sound_off,
        list_keys,
        list_devices,
        print_config,
        dry_run,
        startup_state,
    })
}

/// Open the input device, using an explicit path or by probing available devices.
fn open_device(config: &Config) -> Result<Device> {
    if let Some(path) = &config.device_path {
        let device =
            Device::open(path).with_context(|| format!("Failed to open device {}", path.display()))?;
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

fn play_transition_sound(config: &Config, on: bool) {
    if !config.sounds {
        return;
    }
    if on {
        if let Some(path) = &config.sound_on {
            play_sound_file(path.clone());
        } else {
            play_default_sound(true);
        }
    } else if let Some(path) = &config.sound_off {
        play_sound_file(path.clone());
    } else {
        play_default_sound(false);
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
    if all_pressed != *active {
        set_active_state(config, active, all_pressed)?;
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
        Err(err) => Some(err),
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
        match open_device_with_hint(config) {
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

fn main() -> Result<()> {
    let config = parse_args()?;

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

    let mut device = open_device_with_hint(&config)?;

    println!("🎙 Hold the configured button to talk");

    let mut pressed: HashSet<KeyCode> = HashSet::new();
    let mut active = false;

    while running.load(Ordering::SeqCst) {
        if let Some(err) = handle_events(&config, &mut device, &mut pressed, &mut active)? {
            eprintln!("Input device error: {err}. Reopening...");
            apply_off(&config)?;
            active = false;
            pressed.clear();
            device = reopen_device_loop(&config)?;
        }
    }

    // Final safety mute
    apply_off(&config)?;
    println!("🔇 Mic muted on exit");

    Ok(())
}
