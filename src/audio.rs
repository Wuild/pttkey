use anyhow::{Context, Result};
use rodio::buffer::SamplesBuffer;
use rodio::{Decoder, OutputStreamBuilder, Sample, Sink, Source};
use std::fs;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};

use crate::config::{Config, Mode, SoundChoice};
use crate::constants::{
    DEFAULT_SOUND_OFF_EVENT, DEFAULT_SOUND_OFF_WAV, DEFAULT_SOUND_ON_EVENT, DEFAULT_SOUND_ON_WAV,
};

#[derive(Clone)]
struct PlayRequest {
    samples: SamplesBuffer,
    volume: f32,
}

static AUDIO_SENDER: OnceLock<Sender<PlayRequest>> = OnceLock::new();
static SOUND_CACHE: OnceLock<Mutex<SoundCache>> = OnceLock::new();

fn get_audio_sender() -> Option<&'static Sender<PlayRequest>> {
    if AUDIO_SENDER.get().is_none() {
        let (tx, rx) = mpsc::channel::<PlayRequest>();
        let _ = AUDIO_SENDER.set(tx);
        std::thread::spawn(move || {
            for request in rx {
                let Ok(mut stream) = OutputStreamBuilder::open_default_stream() else {
                    continue;
                };
                stream.log_on_drop(false);
                let sink = Sink::connect_new(stream.mixer());
                sink.set_volume(request.volume);
                sink.append(request.samples);
                sink.sleep_until_end();
            }
        });
    }
    AUDIO_SENDER.get()
}

struct SoundCache {
    on: Option<SamplesBuffer>,
    off: Option<SamplesBuffer>,
    default_on: Option<SamplesBuffer>,
    default_off: Option<SamplesBuffer>,
}

fn decode_samples(bytes: &[u8]) -> Result<SamplesBuffer> {
    let decoder = Decoder::new(BufReader::new(Cursor::new(bytes.to_vec())))
        .context("Failed to decode audio")?;
    let channels = decoder.channels();
    let sample_rate = decoder.sample_rate();
    let samples: Vec<Sample> = decoder.collect();
    Ok(SamplesBuffer::new(channels, sample_rate, samples))
}

fn load_sound_samples(
    choice: &SoundChoice,
    embedded: &'static [u8],
) -> Result<Option<SamplesBuffer>> {
    match choice {
        SoundChoice::Disabled => Ok(None),
        SoundChoice::Default => {
            let samples = decode_samples(embedded)?;
            Ok(Some(samples))
        }
        SoundChoice::File(path) => {
            let bytes = fs::read(path)
                .with_context(|| format!("Failed to read sound file {}", path.display()))?;
            let samples = decode_samples(&bytes)
                .with_context(|| format!("Failed to decode sound file {}", path.display()))?;
            Ok(Some(samples))
        }
    }
}

fn load_default_samples(embedded: &'static [u8]) -> Option<SamplesBuffer> {
    decode_samples(embedded).ok()
}

fn set_sound_cache(cache: SoundCache) {
    if let Some(cell) = SOUND_CACHE.get() {
        if let Ok(mut guard) = cell.lock() {
            *guard = cache;
        }
        return;
    }
    let _ = SOUND_CACHE.set(Mutex::new(cache));
}

fn cached_samples(on: bool) -> Option<SamplesBuffer> {
    let cache = SOUND_CACHE.get()?.lock().ok()?;
    if on {
        cache.on.clone()
    } else {
        cache.off.clone()
    }
}

fn cached_default_samples(on: bool) -> Option<SamplesBuffer> {
    let cache = SOUND_CACHE.get()?.lock().ok()?;
    if on {
        cache.default_on.clone()
    } else {
        cache.default_off.clone()
    }
}

pub(crate) fn init_audio_cache(config: &Config) -> Result<()> {
    if !config.sounds {
        set_sound_cache(SoundCache {
            on: None,
            off: None,
            default_on: None,
            default_off: None,
        });
        return Ok(());
    }

    let on = load_sound_samples(&config.sound_on, DEFAULT_SOUND_ON_WAV)?;
    let off = load_sound_samples(&config.sound_off, DEFAULT_SOUND_OFF_WAV)?;
    let default_on = load_default_samples(DEFAULT_SOUND_ON_WAV);
    let default_off = load_default_samples(DEFAULT_SOUND_OFF_WAV);

    set_sound_cache(SoundCache {
        on,
        off,
        default_on,
        default_off,
    });
    Ok(())
}

fn send_samples(samples: SamplesBuffer, volume: f32) {
    let Some(sender) = get_audio_sender() else {
        return;
    };
    let _ = sender.send(PlayRequest { samples, volume });
}

/// Set the default microphone volume to an absolute level.
pub(crate) fn set_volume(level: f32) -> Result<()> {
    Command::new("wpctl")
        .args(["set-volume", "@DEFAULT_SOURCE@", &format!("{level}")])
        .status()
        .context("wpctl failed")?;
    Ok(())
}

/// Mute or unmute the default microphone source.
pub(crate) fn set_mute(muted: bool) -> Result<()> {
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
fn play_sound_file(path: PathBuf, volume: f32) {
    if let Ok(bytes) = fs::read(&path) {
        if let Ok(samples) = decode_samples(&bytes) {
            send_samples(samples, volume);
        }
    }
}

fn try_play_embedded_sound(on: bool, volume: f32) -> bool {
    let samples = cached_default_samples(on);
    if let Some(samples) = samples {
        send_samples(samples, volume);
        return true;
    }
    false
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
fn play_default_sound(on: bool, volume: f32) {
    std::thread::spawn(move || {
        if try_play_embedded_sound(on, volume) {
            return;
        }
        if try_paplay(on) {
            return;
        }
        let event = if on {
            DEFAULT_SOUND_ON_EVENT
        } else {
            DEFAULT_SOUND_OFF_EVENT
        };
        let _ = try_canberra(event);
    });
}

/// Apply the "mic on" action according to the selected mode.
pub(crate) fn apply_on(config: &Config) -> Result<()> {
    match config.mode {
        Mode::Volume => set_volume(config.on_level),
        Mode::Mute => set_mute(false),
    }
}

/// Apply the "mic off" action according to the selected mode.
pub(crate) fn apply_off(config: &Config) -> Result<()> {
    match config.mode {
        Mode::Volume => set_volume(config.off_level),
        Mode::Mute => set_mute(true),
    }
}

pub(crate) fn play_transition_sound(config: &Config, on: bool) {
    if !config.sounds {
        return;
    }
    if on {
        match &config.sound_on {
            SoundChoice::Default => {
                if let Some(samples) = cached_samples(true) {
                    send_samples(samples, config.sound_volume);
                    return;
                }
                play_default_sound(true, config.sound_volume);
            }
            SoundChoice::Disabled => {}
            SoundChoice::File(path) => {
                if let Some(samples) = cached_samples(true) {
                    send_samples(samples, config.sound_volume);
                    return;
                }
                play_sound_file(path.clone(), config.sound_volume);
            }
        }
        return;
    }

    match &config.sound_off {
        SoundChoice::Default => {
            if let Some(samples) = cached_samples(false) {
                send_samples(samples, config.sound_volume);
                return;
            }
            play_default_sound(false, config.sound_volume);
        }
        SoundChoice::Disabled => {}
        SoundChoice::File(path) => {
            if let Some(samples) = cached_samples(false) {
                send_samples(samples, config.sound_volume);
                return;
            }
            play_sound_file(path.clone(), config.sound_volume);
        }
    }
}
