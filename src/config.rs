use anyhow::{bail, Context, Result};
use evdev::KeyCode;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::constants::{
    CONFIG_BACKUP_NAME, CONFIG_DIR_NAME, CONFIG_FILE_NAME, SUPPORTED_KEYS,
};

/// How the mic is toggled: by absolute volume level or by mute state.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Mode {
    Volume,
    Mute,
}

/// Startup behavior for setting the mic state at launch.
#[derive(Copy, Clone, Debug)]
pub(crate) enum StartupState {
    Muted,
    Unmuted,
}

#[derive(Clone, Debug)]
pub(crate) enum SoundChoice {
    Default,
    Disabled,
    File(PathBuf),
}

/// Runtime configuration assembled from CLI arguments.
#[derive(Clone, Debug)]
pub(crate) struct Config {
    /// Keys that must be held simultaneously to activate the mic.
    pub(crate) keys: Vec<KeyCode>,
    /// Optional explicit input device path (e.g. /dev/input/event7).
    pub(crate) device_path: Option<PathBuf>,
    /// Volume vs mute behavior.
    pub(crate) mode: Mode,
    /// Volume level when active.
    pub(crate) on_level: f32,
    /// Volume level when inactive.
    pub(crate) off_level: f32,
    /// Enable or disable sound effects.
    pub(crate) sounds: bool,
    /// Optional custom sound file for mic on (or disabled).
    pub(crate) sound_on: SoundChoice,
    /// Optional custom sound file for mic off (or disabled).
    pub(crate) sound_off: SoundChoice,
    /// Volume for sound effects (0.0 - 1.0+).
    pub(crate) sound_volume: f32,
    /// Print available keys and exit.
    pub(crate) list_keys: bool,
    /// Print available input devices and exit.
    pub(crate) list_devices: bool,
    /// Print configuration and exit.
    pub(crate) print_config: bool,
    /// Validate inputs and exit without changing mic state.
    pub(crate) dry_run: bool,
    /// Startup mic state.
    pub(crate) startup_state: StartupState,
    /// Reverse behavior so holding keys mutes instead of unmutes.
    pub(crate) reverse: bool,
    /// Suppress configured key events from reaching other apps.
    pub(crate) suppress: bool,
}

/// Config data persisted to disk.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PersistedConfig {
    pub(crate) keys: Vec<String>,
    pub(crate) device_path: Option<String>,
    pub(crate) mode: String,
    pub(crate) on_level: f32,
    pub(crate) off_level: f32,
    pub(crate) sounds: bool,
    pub(crate) sound_on: Option<SoundSettingValue>,
    pub(crate) sound_off: Option<SoundSettingValue>,
    pub(crate) sound_volume: f32,
    pub(crate) startup_state: String,
    pub(crate) reverse: bool,
    pub(crate) suppress: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum SoundSettingValue {
    Bool(bool),
    String(String),
}

impl Default for PersistedConfig {
    fn default() -> Self {
        Self {
            keys: vec!["BTN_EXTRA".to_string()],
            device_path: None,
            mode: "volume".to_string(),
            on_level: 1.0,
            off_level: 0.0,
            sounds: true,
            sound_on: None,
            sound_off: None,
            sound_volume: 1.0,
            startup_state: "muted".to_string(),
            reverse: false,
            suppress: false,
        }
    }
}

pub(crate) fn config_dir() -> Result<PathBuf> {
    if let Ok(path) = env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join(CONFIG_DIR_NAME));
    }
    let home = env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".config").join(CONFIG_DIR_NAME))
}

pub(crate) fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join(CONFIG_FILE_NAME))
}

pub(crate) fn backup_config_path() -> Result<PathBuf> {
    let home = env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(CONFIG_BACKUP_NAME))
}

pub(crate) fn read_persisted_config(path: &Path) -> Result<PersistedConfig> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("Failed to read config {}", path.display()))?;
    toml::from_str(&contents)
        .with_context(|| format!("Failed to parse config {}", path.display()))
}

pub(crate) fn write_persisted_config(
    config: &PersistedConfig,
    primary: &Path,
    backup: &Path,
) -> Result<()> {
    let contents = toml::to_string_pretty(config).context("Failed to serialize config")?;
    if let Some(parent) = primary.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            eprintln!(
                "Warning: failed to create config directory {}: {err}",
                parent.display()
            );
        }
    }

    let mut wrote_primary = false;
    if fs::write(primary, &contents).is_ok() {
        wrote_primary = true;
    } else {
        eprintln!(
            "Warning: failed to write config to {}, falling back to backup path",
            primary.display()
        );
    }

    if !wrote_primary {
        fs::write(backup, &contents).with_context(|| {
            format!(
                "Failed to write backup config to {}",
                backup.display()
            )
        })?;
        return Ok(());
    }

    let _ = fs::write(backup, &contents);
    Ok(())
}

pub(crate) fn load_persisted_config() -> Result<(PersistedConfig, bool, PathBuf)> {
    let primary = config_path()?;
    let backup = backup_config_path()?;

    if primary.exists() {
        return Ok((read_persisted_config(&primary)?, false, primary));
    }
    if backup.exists() {
        let config = read_persisted_config(&backup)?;
        let contents = toml::to_string_pretty(&config).context("Failed to serialize config")?;
        if let Some(parent) = primary.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if fs::write(&primary, &contents).is_ok() {
            let _ = fs::write(&backup, &contents);
            return Ok((config, false, primary));
        }
        return Ok((config, false, backup));
    }

    let config = PersistedConfig::default();
    write_persisted_config(&config, &primary, &backup)?;
    let used = if primary.exists() { primary } else { backup };
    Ok((config, true, used))
}

pub(crate) fn restart_service() {
    let status = Command::new("systemctl")
        .args(["--user", "try-restart", "pttkey.service"])
        .status();
    match status {
        Ok(status) if status.success() => {
            println!("Restarted user service pttkey.service");
        }
        Ok(status) => {
            eprintln!("Warning: failed to restart service (exit {})", status);
        }
        Err(err) => {
            eprintln!("Warning: failed to invoke systemctl: {err}");
        }
    }
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Volume => "volume",
        Mode::Mute => "mute",
    }
}

fn parse_mode(value: &str) -> Result<Mode> {
    match value {
        "volume" => Ok(Mode::Volume),
        "mute" => Ok(Mode::Mute),
        _ => bail!("Invalid --mode '{value}'. Use 'volume' or 'mute'."),
    }
}

fn startup_state_label(state: StartupState) -> &'static str {
    match state {
        StartupState::Muted => "muted",
        StartupState::Unmuted => "unmuted",
    }
}

fn parse_startup_state(value: &str) -> Result<StartupState> {
    match value {
        "muted" => Ok(StartupState::Muted),
        "unmuted" => Ok(StartupState::Unmuted),
        _ => bail!("Invalid --startup-state '{value}'. Use 'muted' or 'unmuted'."),
    }
}

pub(crate) fn persisted_from_config(config: &Config) -> PersistedConfig {
    PersistedConfig {
        keys: config.keys.iter().map(|k| key_label(*k)).collect(),
        device_path: config
            .device_path
            .as_ref()
            .map(|p| p.display().to_string()),
        mode: mode_label(config.mode).to_string(),
        on_level: config.on_level,
        off_level: config.off_level,
        sounds: config.sounds,
        sound_on: sound_setting_value(&config.sound_on),
        sound_off: sound_setting_value(&config.sound_off),
        sound_volume: config.sound_volume,
        startup_state: startup_state_label(config.startup_state).to_string(),
        reverse: config.reverse,
        suppress: config.suppress,
    }
}

pub(crate) fn print_persisted_config(path: &Path, config: &PersistedConfig) {
    let keys = if config.keys.is_empty() {
        "BTN_EXTRA".to_string()
    } else {
        config.keys.join("+")
    };
    println!("config_path: {}", path.display());
    println!("config_keys: {}", keys);
    println!(
        "config_device: {}",
        config
            .device_path
            .as_deref()
            .unwrap_or("auto")
    );
    println!("config_mode: {}", config.mode);
    println!("config_reverse: {}", config.reverse);
    println!("config_on_level: {}", config.on_level);
    println!("config_off_level: {}", config.off_level);
    println!("config_sounds: {}", config.sounds);
    println!(
        "config_sound_on: {}",
        persisted_sound_label(&config.sound_on)
    );
    println!(
        "config_sound_off: {}",
        persisted_sound_label(&config.sound_off)
    );
    println!("config_sound_volume: {}", config.sound_volume);
    println!("config_startup_state: {}", config.startup_state);
    println!("config_suppress: {}", config.suppress);
}

fn sound_setting_value(setting: &SoundChoice) -> Option<SoundSettingValue> {
    match setting {
        SoundChoice::Default => None,
        SoundChoice::Disabled => Some(SoundSettingValue::Bool(false)),
        SoundChoice::File(path) => {
            Some(SoundSettingValue::String(path.display().to_string()))
        }
    }
}

pub(crate) fn persisted_sound_label(setting: &Option<SoundSettingValue>) -> String {
    match setting {
        None => "default".to_string(),
        Some(SoundSettingValue::Bool(false)) => "disabled".to_string(),
        Some(SoundSettingValue::Bool(true)) => "default".to_string(),
        Some(SoundSettingValue::String(value)) => value.clone(),
    }
}

fn sound_label(setting: &SoundChoice) -> String {
    match setting {
        SoundChoice::Default => "default".to_string(),
        SoundChoice::Disabled => "disabled".to_string(),
        SoundChoice::File(path) => path.display().to_string(),
    }
}

fn parse_sound_setting(value: Option<SoundSettingValue>) -> SoundChoice {
    match value {
        None => SoundChoice::Default,
        Some(SoundSettingValue::Bool(false)) => SoundChoice::Disabled,
        Some(SoundSettingValue::Bool(true)) => SoundChoice::Default,
        Some(SoundSettingValue::String(value)) => SoundChoice::File(PathBuf::from(value)),
    }
}

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

    bail!("Unknown key '{input}'. Use a numeric key code or a known name like BTN_EXTRA/KEY_F9.")
}

fn parse_keys(input: &str) -> Result<Vec<KeyCode>> {
    input
        .split('+')
        .map(|part| parse_key(part.trim()))
        .collect()
}

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
  --reverse           invert behavior so holding the key mutes\n\
  --no-reverse        disable reverse behavior\n\
  --on-level <FLOAT>  volume level when pressed (default: 1.0)\n\
  --off-level <FLOAT> volume level when released (default: 0.0)\n\
  --sound-on <PATH>   custom sound file for mic on (mp3/wav/ogg)\n\
  --sound-off <PATH>  custom sound file for mic off (mp3/wav/ogg)\n\
  --sound-volume <FLOAT>  sound volume (default: 1.0)\n\
  --startup-state <muted|unmuted>  initial mic state (default: muted)\n\
  --suppress          suppress only the configured key(s) from reaching other apps\n\
  --no-suppress       do not suppress key events (default)\n\
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
  pttkey --key KEY_F9 --reverse --startup-state unmuted\n\
  pttkey --sound-on ~/on.wav --sound-off ~/off.ogg\n\
  pttkey --device /dev/input/event7 --key KEY_SPACE\n\
\n\
Config:\n\
  ~/.config/pttkey/config.toml (auto-created, CLI updates and restarts service)\n"
    );
}

pub(crate) fn print_supported_keys() {
    for (name, _) in SUPPORTED_KEYS {
        println!("{name}");
    }
}

fn key_label(key: KeyCode) -> String {
    for (name, k) in SUPPORTED_KEYS {
        if *k == key {
            return (*name).to_string();
        }
    }
    format!("{}", key.code())
}

pub(crate) fn print_config(config: &Config) {
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
    let mode = mode_label(config.mode);
    let startup_state = startup_state_label(config.startup_state);
    println!("keys: {keys}");
    println!("device: {device}");
    println!("mode: {mode}");
    println!("reverse: {}", config.reverse);
    println!("on_level: {}", config.on_level);
    println!("off_level: {}", config.off_level);
    println!("sounds: {}", config.sounds);
    println!("sound_on: {}", sound_label(&config.sound_on));
    println!("sound_off: {}", sound_label(&config.sound_off));
    println!("sound_volume: {}", config.sound_volume);
    println!("startup_state: {startup_state}");
    println!("suppress: {}", config.suppress);
}

pub(crate) fn config_from_persisted(base: PersistedConfig) -> Result<Config> {
    let mut keys: Vec<KeyCode> = base
        .keys
        .iter()
        .map(|k| parse_key(k))
        .collect::<Result<Vec<_>>>()?;
    if keys.is_empty() {
        keys.push(KeyCode::BTN_EXTRA);
    }

    let device_path = base.device_path.map(PathBuf::from);
    let mode = parse_mode(&base.mode)?;
    let reverse = base.reverse;
    let on_level = base.on_level;
    let off_level = base.off_level;
    let sounds = base.sounds;
    let sound_on = parse_sound_setting(base.sound_on);
    let sound_off = parse_sound_setting(base.sound_off);
    let sound_volume = base.sound_volume;
    let startup_state = parse_startup_state(&base.startup_state)?;
    let suppress = base.suppress;

    if let SoundChoice::File(path) = &sound_on {
        if !path.exists() {
            bail!("Sound on file does not exist: {}", path.display());
        }
    }
    if let SoundChoice::File(path) = &sound_off {
        if !path.exists() {
            bail!("Sound off file does not exist: {}", path.display());
        }
    }

    Ok(Config {
        keys,
        device_path,
        mode,
        reverse,
        on_level,
        off_level,
        sounds,
        sound_on,
        sound_off,
        sound_volume,
        list_keys: false,
        list_devices: false,
        print_config: false,
        dry_run: false,
        startup_state,
        suppress,
    })
}

pub(crate) fn parse_args(base: PersistedConfig) -> Result<(Config, bool)> {
    let mut keys: Vec<KeyCode> = base
        .keys
        .iter()
        .map(|k| parse_key(k))
        .collect::<Result<Vec<_>>>()?;
    if keys.is_empty() {
        keys.push(KeyCode::BTN_EXTRA);
    }
    let mut device_path = base.device_path.map(PathBuf::from);
    let mut mode = parse_mode(&base.mode)?;
    let mut reverse = base.reverse;
    let mut on_level = base.on_level;
    let mut off_level = base.off_level;
    let mut sounds = base.sounds;
    let mut sound_on = parse_sound_setting(base.sound_on);
    let mut sound_off = parse_sound_setting(base.sound_off);
    let mut sound_volume = base.sound_volume;
    let mut list_keys = false;
    let mut list_devices = false;
    let mut print_config = false;
    let mut dry_run = false;
    let mut startup_state = parse_startup_state(&base.startup_state)?;
    let mut startup_state_set = false;
    let mut suppress = base.suppress;
    let mut persist_changed = false;
    let mut key_set = false;

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
                if !key_set {
                    keys.clear();
                    key_set = true;
                }
                keys.append(&mut parsed);
                persist_changed = true;
            }
            "--device" => {
                i += 1;
                let value = args.get(i).context("missing value for --device")?;
                device_path = Some(PathBuf::from(value));
                persist_changed = true;
            }
            "--mode" => {
                i += 1;
                let value = args.get(i).context("missing value for --mode")?;
                mode = parse_mode(value)?;
                persist_changed = true;
            }
            "--reverse" => {
                reverse = true;
                persist_changed = true;
            }
            "--no-reverse" => {
                reverse = false;
                persist_changed = true;
            }
            "--on-level" => {
                i += 1;
                let value = args.get(i).context("missing value for --on-level")?;
                on_level = value
                    .parse::<f32>()
                    .with_context(|| format!("invalid --on-level '{value}'"))?;
                persist_changed = true;
            }
            "--off-level" => {
                i += 1;
                let value = args.get(i).context("missing value for --off-level")?;
                off_level = value
                    .parse::<f32>()
                    .with_context(|| format!("invalid --off-level '{value}'"))?;
                persist_changed = true;
            }
            "--sound-on" => {
                i += 1;
                let value = args.get(i).context("missing value for --sound-on")?;
                if value.eq_ignore_ascii_case("false") || value == "0" {
                    sound_on = SoundChoice::Disabled;
                } else {
                    sound_on = SoundChoice::File(PathBuf::from(value));
                }
                persist_changed = true;
            }
            "--sound-off" => {
                i += 1;
                let value = args.get(i).context("missing value for --sound-off")?;
                if value.eq_ignore_ascii_case("false") || value == "0" {
                    sound_off = SoundChoice::Disabled;
                } else {
                    sound_off = SoundChoice::File(PathBuf::from(value));
                }
                persist_changed = true;
            }
            "--sound-volume" => {
                i += 1;
                let value = args.get(i).context("missing value for --sound-volume")?;
                sound_volume = value
                    .parse::<f32>()
                    .with_context(|| format!("invalid --sound-volume '{value}'"))?;
                persist_changed = true;
            }
            "--startup-state" => {
                i += 1;
                let value = args.get(i).context("missing value for --startup-state")?;
                startup_state = parse_startup_state(value)?;
                startup_state_set = true;
                persist_changed = true;
            }
            "--sounds" => {
                sounds = true;
                persist_changed = true;
            }
            "--no-sounds" => {
                sounds = false;
                persist_changed = true;
            }
            "--suppress" => {
                suppress = true;
                persist_changed = true;
            }
            "--no-suppress" => {
                suppress = false;
                persist_changed = true;
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

    if let SoundChoice::File(path) = &sound_on {
        if !path.exists() {
            bail!("Sound on file does not exist: {}", path.display());
        }
    }
    if let SoundChoice::File(path) = &sound_off {
        if !path.exists() {
            bail!("Sound off file does not exist: {}", path.display());
        }
    }

    if reverse && !startup_state_set {
        startup_state = StartupState::Unmuted;
    }

    Ok((
        Config {
            keys,
            device_path,
            mode,
            reverse,
            on_level,
            off_level,
            sounds,
            sound_on,
            sound_off,
            sound_volume,
            list_keys,
            list_devices,
            print_config,
            dry_run,
            startup_state,
            suppress,
        },
        persist_changed,
    ))
}
