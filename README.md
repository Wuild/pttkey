# pttkey

Push-to-talk mic toggle for PipeWire. Hold a mouse or keyboard button to
unmute the default microphone source via `wpctl`.

## Requirements

- Linux with PipeWire and `wpctl` available in PATH
- An input device (mouse or keyboard) with a usable key/button
- Rust toolchain (for building)

## Build

```
cargo build --release
```

## Usage

```
pttkey --key BTN_EXTRA
pttkey --key KEY_F9 --mode mute --no-sounds
pttkey --key KEY_LEFTCTRL+KEY_F --mode mute
pttkey --sound-on ~/on.wav --sound-off ~/off.ogg
pttkey --device /dev/input/event7 --key KEY_SPACE
pttkey --list-devices
pttkey --list-keys
```

## Config

On first run, a config file is created at `~/.config/pttkey/config.toml`.
CLI flags update the config and trigger a user service restart.
If the config directory cannot be written, a backup is stored at
`~/.pttkey-config.toml`.

### Options

| Argument | Meaning | Default / Notes |
| --- | --- | --- |
| `--key <NAME\|CODE>` | Evdev key name or numeric code. Can be repeated or combined with `+` for chords (e.g. `--key KEY_LEFTCTRL+KEY_F`). | Default: `BTN_EXTRA` |
| `--device <PATH>` | Input device path to use instead of auto-detect. | Optional |
| `--mode <volume\|mute>` | Control by volume level or `set-mute`. | Default: `volume` |
| `--reverse` | Invert behavior so holding the key mutes. | Optional |
| `--no-reverse` | Disable reverse behavior (normal push-to-talk). | Optional |
| `--on-level <FLOAT>` | Volume when pressed. | Default: `1.0` |
| `--off-level <FLOAT>` | Volume when released. | Default: `0.0` |
| `--sound-on <PATH>` | Custom sound file for mic on (`mp3`, `wav`, `ogg`). | Optional |
| `--sound-off <PATH>` | Custom sound file for mic off (`mp3`, `wav`, `ogg`). | Optional |
| `--startup-state <muted\|unmuted>` | Initial mic state. | Default: `muted` |
| `--sounds` | Enable on/off sounds using system default sounds. | Enabled by default |
| `--no-sounds` | Disable on/off sounds. | Overrides `--sounds` |
| `--list-keys` | Print supported key names and exit. |  |
| `--list-devices` | Print input devices and exit. |  |
| `--print-config` | Print parsed configuration and exit. |  |
| `--dry-run` | Validate configuration and exit without changing mic state. |  |

### Supported key names

Usual keyboard and mouse keys are supported by name. You can also use numeric codes.

- Mouse: `BTN_LEFT`, `BTN_RIGHT`, `BTN_MIDDLE`, `BTN_SIDE`, `BTN_EXTRA`, `BTN_FORWARD`, `BTN_BACK`
- Letters: `KEY_A` through `KEY_Z`
- Numbers: `KEY_0` through `KEY_9`
- Function keys: `KEY_F1` through `KEY_F12`
- Modifiers: `KEY_LEFTCTRL`, `KEY_RIGHTCTRL`, `KEY_LEFTSHIFT`, `KEY_RIGHTSHIFT`, `KEY_LEFTALT`, `KEY_RIGHTALT`, `KEY_LEFTMETA`, `KEY_RIGHTMETA`
- Whitespace/controls: `KEY_SPACE`, `KEY_TAB`, `KEY_ENTER`, `KEY_ESC`, `KEY_BACKSPACE`, `KEY_CAPSLOCK`
- Navigation: `KEY_UP`, `KEY_DOWN`, `KEY_LEFT`, `KEY_RIGHT`, `KEY_HOME`, `KEY_END`, `KEY_PAGEUP`, `KEY_PAGEDOWN`, `KEY_INSERT`, `KEY_DELETE`
- Punctuation: `KEY_MINUS`, `KEY_EQUAL`, `KEY_LEFTBRACE`, `KEY_RIGHTBRACE`, `KEY_BACKSLASH`, `KEY_SEMICOLON`, `KEY_APOSTROPHE`, `KEY_GRAVE`, `KEY_COMMA`, `KEY_DOT`, `KEY_SLASH`
- Numpad: `KEY_NUMLOCK`, `KEY_KPSLASH`, `KEY_KPASTERISK`, `KEY_KPMINUS`, `KEY_KPPLUS`, `KEY_KPENTER`, `KEY_KP0`-`KEY_KP9`, `KEY_KPDOT`
- Media: `KEY_MUTE`, `KEY_VOLUMEDOWN`, `KEY_VOLUMEUP`, `KEY_PLAYPAUSE`, `KEY_NEXTSONG`, `KEY_PREVIOUSSONG`, `KEY_STOPCD`

Use `pttkey --list-keys` to print the exact list accepted by the current build.

## Install (user service)

You can also use `./install.sh` to build, install, and set up the user service.

1) Copy the service file to your user systemd directory:

```
mkdir -p ~/.config/systemd/user
cp pttkey.service ~/.config/systemd/user/
```

2) Update the `ExecStart` path in `~/.config/systemd/user/pttkey.service` if
   your checkout lives elsewhere, and add any flags you want (e.g. `--key KEY_F9 --mode mute`).

3) Enable and start the service:

```
systemctl --user daemon-reload
systemctl --user enable --now pttkey.service
```

## Notes

- If the service cannot read your input device, add your user to the `input`
  group and re-login:

```
sudo usermod -aG input $USER
```

- Install the bundled udev rule (sets input devices to `GROUP="input"` and `MODE="0660"`):

```
sudo install -m 644 99-ptt-input.rules /etc/udev/rules.d/99-ptt-input.rules
sudo udevadm control --reload-rules
sudo udevadm trigger
```

- Default sounds are bundled (`mute.wav`/`unmute.wav`); if they fail to play, `paplay` or `canberra-gtk-play` is used as fallback.

- Stop the service with:

```
systemctl --user stop pttkey.service
```

## Release checklist

1) Update `Cargo.toml` version and `CHANGELOG.md`.
2) Build and test: `cargo build --release` and `cargo test`.
3) Tag and push: `git tag -a vX.Y.Z -m "vX.Y.Z" && git push --tags`.
