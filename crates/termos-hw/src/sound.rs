// SPDX-License-Identifier: GPL-3.0-or-later
//! Sound on a TermOS image (M13e, D-127, D-128): the volume and mute of
//! the default output, the outputs there are (Bluetooth headsets among
//! them), and a short beep for the bell.
//!
//! The image runs PipeWire with WirePlumber. TermOS asks WirePlumber's own
//! tool, `wpctl`, rather than speaking PipeWire's protocol: its output is
//! small and has stayed the same across releases, and reading it is all
//! this module does ([`parse_volume`], [`parse_sinks`]; a fuzz target reads
//! both). Every call runs a program and waits for it, some milliseconds:
//! make them off the event loop.

use std::io;
use std::process::{Command, Stdio};

/// The default output, as `wpctl` names it.
pub const DEFAULT_SINK: &str = "@DEFAULT_AUDIO_SINK@";

/// The loudest TermOS sets an output: 100%. PipeWire allows more, which
/// distorts; another program may still set it.
pub const LIMIT: f32 = 1.0;

/// An output's volume.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Volume {
    /// 0 to 1 (more when another program set it past 100%).
    pub level: f32,
    pub muted: bool,
}

impl Volume {
    /// The level in percent.
    pub fn percent(&self) -> u16 {
        (self.level.max(0.0) * 100.0)
            .round()
            .min(f32::from(u16::MAX)) as u16
    }

    /// For the status line: `◁ 45%`, `◁ muted`. (No speaker emoji: many
    /// terminals draw those two cells wide.)
    pub fn status_text(&self) -> String {
        if self.muted {
            "◁ muted".to_owned()
        } else {
            format!("◁ {}%", self.percent())
        }
    }

    /// For a message: `volume 45%`, `muted (45%)`.
    pub fn message(&self) -> String {
        if self.muted {
            format!("muted ({}%)", self.percent())
        } else {
            format!("volume {}%", self.percent())
        }
    }
}

/// An output PipeWire offers: speakers, HDMI, a Bluetooth headset…
#[derive(Debug, Clone, PartialEq)]
pub struct Sink {
    /// PipeWire's object id (what `wpctl` takes).
    pub id: u32,
    pub name: String,
    /// The default output.
    pub default: bool,
    pub volume: Option<Volume>,
}

/// `wpctl get-volume`'s answer: `Volume: 0.40`, `Volume: 0.40 [MUTED]`.
pub fn parse_volume(out: &str) -> Option<Volume> {
    let rest = out.trim().strip_prefix("Volume:")?;
    let level: f32 = rest.split_whitespace().next()?.parse().ok()?;
    if !level.is_finite() || level < 0.0 {
        return None;
    }
    Some(Volume {
        level,
        muted: rest.contains("[MUTED]"),
    })
}

/// The audio outputs in `wpctl status`: the lines under `Audio`, `Sinks:`.
///
/// ```text
/// Audio
///  ├─ Sinks:
///  │  *   50. Built-in Audio Analog Stereo        [vol: 0.40]
///  │      51. WH-1000XM4                          [vol: 1.00 MUTED]
///  │
///  ├─ Sources:
/// ```
pub fn parse_sinks(status: &str) -> Vec<Sink> {
    let mut sinks = Vec::new();
    let mut audio = false;
    let mut in_sinks = false;
    for line in status.lines() {
        // A section: a line that starts with neither space nor a tree.
        if line.chars().next().is_some_and(|c| !c.is_whitespace()) {
            audio = line.trim() == "Audio";
            in_sinks = false;
            continue;
        }
        if !audio {
            continue;
        }
        let body = line.trim_start_matches(|c: char| c.is_whitespace() || "│├└─".contains(c));
        // A subsection: `├─ Sinks:`.
        if line.contains('─') && body.ends_with(':') {
            in_sinks = body == "Sinks:";
            continue;
        }
        if !in_sinks {
            continue;
        }
        if let Some(sink) = sink_line(body) {
            sinks.push(sink);
        }
    }
    sinks
}

/// `*   50. Built-in Audio Analog Stereo        [vol: 0.40]`.
fn sink_line(body: &str) -> Option<Sink> {
    let (default, body) = match body.strip_prefix('*') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, body),
    };
    let (id, rest) = body.split_once('.')?;
    let id: u32 = id.trim().parse().ok()?;
    let rest = rest.trim();
    let (name, volume) = match rest.rfind('[') {
        Some(at) if rest[at..].starts_with("[vol:") => {
            let inner = rest[at + 5..].trim_end().trim_end_matches(']');
            let mut words = inner.split_whitespace();
            let level = words.next().and_then(|l| l.parse::<f32>().ok());
            let muted = words.any(|w| w == "MUTED");
            (
                rest[..at].trim(),
                level
                    .filter(|l| l.is_finite() && *l >= 0.0)
                    .map(|level| Volume { level, muted }),
            )
        }
        _ => (rest, None),
    };
    if name.is_empty() {
        return None;
    }
    Some(Sink {
        id,
        name: name.to_owned(),
        default,
        volume,
    })
}

/// Runs `wpctl ARGS` and returns what it printed.
fn wpctl(args: &[&str]) -> io::Result<String> {
    let out = Command::new("wpctl")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => io::Error::new(
                io::ErrorKind::NotFound,
                "no sound: PipeWire's wpctl is not installed",
            ),
            _ => io::Error::new(e.kind(), format!("cannot run wpctl: {e}")),
        })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(io::Error::other(if err.is_empty() {
            format!("wpctl {} failed ({})", args.join(" "), out.status)
        } else {
            err
        }));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The volume of `sink` (an id, or [`DEFAULT_SINK`]).
pub fn volume_of(sink: &str) -> io::Result<Volume> {
    let out = wpctl(&["get-volume", sink])?;
    parse_volume(&out).ok_or_else(|| io::Error::other(format!("wpctl said `{}`", out.trim())))
}

/// The default output's volume.
pub fn volume() -> io::Result<Volume> {
    volume_of(DEFAULT_SINK)
}

/// Turns the default output up or down by `delta` percent (never past
/// 100%); turning it up unmutes it. Returns the new volume.
pub fn change_volume(delta: i8) -> io::Result<Volume> {
    change_volume_of(DEFAULT_SINK, delta)
}

/// [`change_volume`] for `sink` (an id, or [`DEFAULT_SINK`]).
pub fn change_volume_of(sink: &str, delta: i8) -> io::Result<Volume> {
    let step = format!(
        "{}%{}",
        delta.unsigned_abs(),
        if delta < 0 { '-' } else { '+' }
    );
    let limit = LIMIT.to_string();
    wpctl(&["set-volume", "-l", &limit, sink, &step])?;
    if delta > 0 {
        wpctl(&["set-mute", sink, "0"])?;
    }
    volume_of(sink)
}

/// Sets `sink`'s volume to `percent` (at most 100).
pub fn set_volume(sink: &str, percent: u8) -> io::Result<Volume> {
    let level = format!("{:.2}", f32::from(percent.min(100)) / 100.0);
    wpctl(&["set-volume", sink, &level])?;
    volume_of(sink)
}

/// Mutes the default output, or unmutes it.
pub fn toggle_mute() -> io::Result<Volume> {
    toggle_mute_of(DEFAULT_SINK)
}

/// [`toggle_mute`] for `sink`.
pub fn toggle_mute_of(sink: &str) -> io::Result<Volume> {
    wpctl(&["set-mute", sink, "toggle"])?;
    volume_of(sink)
}

/// At start: makes the output `[sound] device` names the default, if it
/// is there, and returns the default output's volume.
pub fn start(device: &str) -> io::Result<Volume> {
    if !device.is_empty() {
        let all = sinks()?;
        match find_sink(&all, device) {
            Some(s) if !s.default => set_default(s.id)?,
            Some(_) => {}
            None => tracing::info!("no sound output named {device:?}: keeping the default"),
        }
    }
    volume()
}

/// The outputs, the default marked.
pub fn sinks() -> io::Result<Vec<Sink>> {
    Ok(parse_sinks(&wpctl(&["status"])?))
}

/// Makes output `id` the default; what plays moves to it.
pub fn set_default(id: u32) -> io::Result<()> {
    wpctl(&["set-default", &id.to_string()]).map(drop)
}

/// The default output by name (`[sound] device`): the first output whose
/// name contains `name`, ignoring case.
pub fn find_sink<'a>(sinks: &'a [Sink], name: &str) -> Option<&'a Sink> {
    let name = name.to_lowercase();
    sinks
        .iter()
        .find(|s| s.name.to_lowercase() == name)
        .or_else(|| sinks.iter().find(|s| s.name.to_lowercase().contains(&name)))
}

/// A short tone as a WAV file: 16-bit mono at `rate`, faded in and out so
/// it does not click.
pub fn tone(hz: f32, ms: u32, rate: u32) -> Vec<u8> {
    let frames = (rate as u64 * u64::from(ms) / 1000) as u32;
    let fade = (rate / 200).max(1).min(frames / 2).max(1);
    let data_len = frames * 2;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        let t = i as f32 / rate as f32;
        let edge = i.min(frames - 1 - i);
        let gain = (edge as f32 / fade as f32).min(1.0) * 0.3;
        let s = (t * hz * std::f32::consts::TAU).sin() * gain;
        w.extend_from_slice(&((s * f32::from(i16::MAX)) as i16).to_le_bytes());
    }
    w
}

/// Beeps once (the bell, `[kms] audible-bell`) through `pw-play`, without
/// waiting; a beep while one plays is dropped, so a burst of bells is one
/// beep.
pub fn beep() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static PLAYING: AtomicBool = AtomicBool::new(false);
    if PLAYING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("termos-beep".into())
        .spawn(|| {
            if let Err(e) = play_beep() {
                tracing::debug!("no beep: {e}");
            }
            PLAYING.store(false, Ordering::Release);
        });
    if let Err(e) = spawned {
        PLAYING.store(false, Ordering::Release);
        tracing::debug!("no beep: {e}");
    }
}

fn play_beep() -> io::Result<()> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let file = dir.join("termos-bell.wav");
    if !file.exists() {
        std::fs::write(&file, tone(880.0, 120, 48_000))?;
    }
    let status = Command::new("pw-play")
        .arg(&file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("pw-play failed ({status})")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volumes_are_read() {
        assert_eq!(
            parse_volume("Volume: 0.40\n"),
            Some(Volume {
                level: 0.4,
                muted: false
            })
        );
        assert_eq!(
            parse_volume("Volume: 1.00 [MUTED]"),
            Some(Volume {
                level: 1.0,
                muted: true
            })
        );
        assert_eq!(parse_volume("Translate ID error: 999"), None);
        assert_eq!(parse_volume("Volume: -1"), None);
        assert_eq!(parse_volume("Volume: NaN"), None);
        let v = parse_volume("Volume: 0.456").expect("read");
        assert_eq!(v.percent(), 46);
        assert_eq!(v.status_text(), "◁ 46%");
        assert_eq!(
            Volume {
                level: 0.8,
                muted: true
            }
            .message(),
            "muted (80%)"
        );
    }

    const STATUS: &str = "\
PipeWire 'pipewire-0' [1.0.5, termos@termos, cookie:1234]
 └─ Clients:
        31. WirePlumber                         [1.0.5, termos@termos, pid:300]

Audio
 ├─ Devices:
 │      42. Built-in Audio                      [alsa]
 │      43. WH-1000XM4                          [bluez5]
 │
 ├─ Sinks:
 │      50. Built-in Audio Analog Stereo        [vol: 0.40]
 │  *   51. WH-1000XM4                          [vol: 1.00 MUTED]
 │      52. HDMI / DisplayPort 1 Output
 │
 ├─ Sink endpoints:
 │
 ├─ Sources:
 │  *   53. Built-in Audio Analog Stereo        [vol: 1.00]
 │
 └─ Streams:
        60. termos-bell
             61. output_FL       > WH-1000XM4:playback_FL	[active]

Video
 ├─ Devices:
 │      70. HD Webcam                           [v4l2]
 ├─ Sinks:
 │      71. Not audio                           [vol: 1.00]

Settings
 └─ Default Configured Node Names:
         0. Audio/Sink    bluez_output.AC_80_0A.1
";

    #[test]
    fn outputs_are_listed_from_wpctl_status() {
        let sinks = parse_sinks(STATUS);
        assert_eq!(sinks.len(), 3, "{sinks:#?}");
        assert_eq!(sinks[0].id, 50);
        assert_eq!(sinks[0].name, "Built-in Audio Analog Stereo");
        assert!(!sinks[0].default);
        assert_eq!(sinks[0].volume.map(|v| v.percent()), Some(40));
        assert_eq!(sinks[1].name, "WH-1000XM4");
        assert!(sinks[1].default);
        assert_eq!(sinks[1].volume.map(|v| v.muted), Some(true));
        assert_eq!(sinks[2].name, "HDMI / DisplayPort 1 Output");
        assert_eq!(sinks[2].volume, None);
        assert!(parse_sinks("").is_empty());
        assert!(parse_sinks("Audio\n ├─ Sinks:\n │  *   x. nothing\n").is_empty());
        assert_eq!(find_sink(&sinks, "wh-1000xm4").map(|s| s.id), Some(51));
        assert_eq!(find_sink(&sinks, "hdmi").map(|s| s.id), Some(52));
        assert_eq!(find_sink(&sinks, "usb"), None);
    }

    #[test]
    fn a_tone_is_a_wav_file() {
        let w = tone(880.0, 100, 8000);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        let frames = 800;
        assert_eq!(w.len(), 44 + frames * 2);
        assert_eq!(
            u32::from_le_bytes([w[40], w[41], w[42], w[43]]) as usize,
            frames * 2
        );
        // Starts and ends silent.
        assert_eq!(&w[44..46], &[0, 0]);
        assert!(tone(440.0, 0, 8000).len() == 44);
    }
}
