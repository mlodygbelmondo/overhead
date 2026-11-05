//! Playing the recording, optionally through a frequency boost (the
//! reference video boosts the band of a faint propeller to hear it): play,
//! pause, seek, an end point, live boost/gain changes and the playing
//! position for a timeline cursor. The DSP is `overhead_core::listen`; this
//! module only connects it to the default output device with cpal (WASAPI
//! on Windows, ALSA on Linux, CoreAudio on macOS). No UI here.
//!
//! The cpal stream lives on its own thread (on some platforms it must not
//! move between threads), so a [`Player`] is `Send + Sync` and can sit in
//! a GPUI entity. The audio callback never blocks: if the UI holds the lock
//! at that moment, it plays one buffer of silence.

// The UI builds on this; until it does, parts of the API are unused.
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use overhead_core::audio::Audio;
use overhead_core::listen::{ListenSettings, Renderer};

/// Most samples kept for listening (f32: 4 bytes each, so about 190 MB).
pub const LISTEN_MAX_SAMPLES: f64 = 48e6;

/// Sample rate to decode a recording at for listening. The analysis audio is
/// 8 kHz ([`overhead_core::audio::DEFAULT_TARGET_RATE`]), which is enough for
/// propeller lines but sounds like a telephone (nothing above 4 kHz). This
/// gives up to 24 kHz (sound up to 12 kHz) within [`LISTEN_MAX_SAMPLES`]:
/// 24 kHz up to about 33 minutes, then less (1 h: about 13 kHz; from about
/// 100 minutes on the analysis rate itself, so a 2 h recording still plays at
/// 8 kHz), and never above the file's own rate.
pub fn listen_rate(duration_s: f64, source_rate: u32) -> u32 {
    let budget = (LISTEN_MAX_SAMPLES / duration_s.max(1.0)) as u32;
    budget.clamp(overhead_core::audio::DEFAULT_TARGET_RATE, 24_000).min(source_rate.max(overhead_core::audio::DEFAULT_TARGET_RATE))
}

/// The recording for the player: decoded again at [`listen_rate`] when that
/// is clearly better than the analysis audio, otherwise the analysis audio
/// itself (no second decode). Slow for long files (a full decode): call it
/// off the UI thread.
pub fn load_for_listening(analysis: &Arc<Audio>) -> Result<Arc<Audio>> {
    let rate = listen_rate(analysis.duration(), analysis.source_rate);
    if (rate as f64) < 1.25 * analysis.sample_rate {
        return Ok(analysis.clone());
    }
    Ok(Arc::new(overhead_core::audio::load(&analysis.source, rate)?))
}

/// What the audio callback and the UI share.
pub struct Shared {
    audio: Arc<Audio>,
    renderer: Mutex<Renderer>,
    playing: AtomicBool,
    /// Playing position (s) as f64 bits.
    position: AtomicU64,
    /// The end point was reached (cleared by play/seek).
    ended: AtomicBool,
    error: Mutex<Option<String>>,
}

impl Shared {
    fn new(audio: Arc<Audio>, out_rate: f64) -> Self {
        let mut r = Renderer::new(ListenSettings::default(), audio.sample_rate, out_rate);
        r.seek(0.0, &audio.samples);
        Self {
            audio,
            renderer: Mutex::new(r),
            playing: AtomicBool::new(false),
            position: AtomicU64::new(0f64.to_bits()),
            ended: AtomicBool::new(false),
            error: Mutex::new(None),
        }
    }

    /// The audio callback's work: `out` interleaved f32 with `channels` per frame.
    fn fill(&self, out: &mut [f32], channels: usize) {
        if !self.playing.load(Ordering::Acquire) {
            out.fill(0.0);
            return;
        }
        let Ok(mut r) = self.renderer.try_lock() else {
            out.fill(0.0);
            return;
        };
        let frames = out.len() / channels.max(1);
        let w = r.render(&self.audio.samples, out, channels);
        self.position.store(r.position().to_bits(), Ordering::Release);
        if w < frames || r.at_end(&self.audio.samples) {
            self.playing.store(false, Ordering::Release);
            self.ended.store(true, Ordering::Release);
        }
    }

    fn set_position(&self, t: f64) {
        self.position.store(t.to_bits(), Ordering::Release);
    }
}

/// Plays one recording on the default output device.
pub struct Player {
    shared: Arc<Shared>,
    /// Dropping it stops the stream thread.
    stop: Option<mpsc::Sender<()>>,
    threads: Vec<std::thread::JoinHandle<()>>,
    /// Cleared on drop: the position thread ends.
    alive: Arc<AtomicBool>,
    /// Output device name and rate, for the UI.
    pub device: String,
    pub out_rate: u32,
}

impl Player {
    /// Open the default output device for `audio` (paused at 0 s). Fails
    /// when there is no output device or it cannot be opened.
    pub fn new(audio: Arc<Audio>) -> Result<Self> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(Arc<Shared>, String, u32)>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let t = std::thread::Builder::new()
            .name("overhead-audio".into())
            .spawn(move || {
                let opened = open(audio);
                match opened {
                    Ok((stream, shared, name, rate)) => {
                        let _ = ready_tx.send(Ok((shared, name, rate)));
                        // keep the stream alive until the player is dropped
                        let _ = stop_rx.recv();
                        drop(stream);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .context("audio thread")?;
        let (shared, device, out_rate) = ready_rx.recv_timeout(Duration::from_secs(10)).map_err(|_| anyhow!("the audio device did not open"))??;
        Ok(Self { shared, stop: Some(stop_tx), threads: vec![t], alive: Arc::new(AtomicBool::new(true)), device, out_rate })
    }

    pub fn duration(&self) -> f64 {
        self.shared.audio.duration()
    }

    /// Start (or continue) playing from the current position.
    pub fn play(&self) {
        let r = self.shared.renderer.lock().unwrap();
        if r.at_end(&self.shared.audio.samples) {
            return;
        }
        drop(r);
        self.shared.ended.store(false, Ordering::Release);
        self.shared.playing.store(true, Ordering::Release);
    }

    pub fn pause(&self) {
        self.shared.playing.store(false, Ordering::Release);
    }

    pub fn toggle(&self) {
        if self.is_playing() { self.pause() } else { self.play() }
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Acquire)
    }

    /// Playback stopped at the end point (or the end of the recording).
    pub fn ended(&self) -> bool {
        self.shared.ended.load(Ordering::Acquire)
    }

    /// Move to `t` s (playing or not).
    pub fn seek(&self, t: f64) {
        let t = t.clamp(0.0, self.duration());
        let mut r = self.shared.renderer.lock().unwrap();
        r.seek(t, &self.shared.audio.samples);
        self.shared.set_position(t);
        self.shared.ended.store(false, Ordering::Release);
    }

    /// Seek to `t` and play until `end` (s; `None` = the end of the recording).
    pub fn play_from(&self, t: f64, end: Option<f64>) {
        {
            let mut r = self.shared.renderer.lock().unwrap();
            r.set_end(end, &self.shared.audio.samples);
        }
        self.seek(t);
        self.play();
    }

    /// Stop at `end` s (`None` = the end of the recording).
    pub fn set_end(&self, end: Option<f64>) {
        self.shared.renderer.lock().unwrap().set_end(end, &self.shared.audio.samples);
    }

    /// Boost band, levels and gain; applied from the next buffer on.
    pub fn set_settings(&self, s: ListenSettings) {
        self.shared.renderer.lock().unwrap().set_settings(s);
    }

    pub fn settings(&self) -> ListenSettings {
        self.shared.renderer.lock().unwrap().settings()
    }

    /// The position being played (s).
    pub fn position(&self) -> f64 {
        f64::from_bits(self.shared.position.load(Ordering::Acquire))
    }

    /// Last stream error (device unplugged, …), if any.
    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap().clone()
    }

    /// Call `cb(position, playing)` about 30 times a second while playing
    /// (and once when it stops), from a helper thread, until the player is
    /// dropped. For a timeline cursor: e.g. send to the UI and redraw.
    pub fn on_position(&mut self, cb: impl Fn(f64, bool) + Send + 'static) {
        let s = self.shared.clone();
        let alive = self.alive.clone();
        let t = std::thread::Builder::new().name("overhead-audio-pos".into()).spawn(move || {
            let mut was = false;
            while alive.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(33));
                let playing = s.playing.load(Ordering::Acquire);
                if playing || was {
                    cb(f64::from_bits(s.position.load(Ordering::Acquire)), playing);
                }
                was = playing;
            }
        });
        if let Ok(t) = t {
            self.threads.push(t);
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shared.playing.store(false, Ordering::Release);
        self.alive.store(false, Ordering::Release);
        // the stream thread ends at once, the position thread within one tick
        self.stop.take();
        for t in std::mem::take(&mut self.threads) {
            let _ = t.join();
        }
    }
}

/// Open the default output device and build a stream that plays `audio`.
fn open(audio: Arc<Audio>) -> Result<(cpal::Stream, Arc<Shared>, String, u32)> {
    let host = cpal::default_host();
    let device = host.default_output_device().context("no audio output device")?;
    let name = device.description().map(|d| d.to_string()).unwrap_or_else(|_| "default output".into());
    let supported = device.default_output_config().context("audio output configuration")?;
    let rate = supported.sample_rate();
    let channels = supported.channels() as usize;
    let format = supported.sample_format();
    let config = supported.config();
    let shared = Arc::new(Shared::new(audio, rate as f64));
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, config, channels, shared.clone())?,
        SampleFormat::F64 => build::<f64>(&device, config, channels, shared.clone())?,
        SampleFormat::I16 => build::<i16>(&device, config, channels, shared.clone())?,
        SampleFormat::I32 => build::<i32>(&device, config, channels, shared.clone())?,
        SampleFormat::U16 => build::<u16>(&device, config, channels, shared.clone())?,
        SampleFormat::U8 => build::<u8>(&device, config, channels, shared.clone())?,
        SampleFormat::I8 => build::<i8>(&device, config, channels, shared.clone())?,
        other => return Err(anyhow!("unsupported output sample format {other:?}")),
    };
    stream.play().context("start audio output")?;
    Ok((stream, shared, name, rate))
}

fn build<T>(device: &cpal::Device, config: cpal::StreamConfig, channels: usize, shared: Arc<Shared>) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let err_shared = shared.clone();
    let mut buf: Vec<f32> = Vec::new();
    let stream = device.build_output_stream(
        config,
        move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
            buf.resize(out.len(), 0.0);
            shared.fill(&mut buf, channels);
            for (o, v) in out.iter_mut().zip(&buf) {
                *o = T::from_sample(*v);
            }
        },
        move |e| {
            if let Ok(mut g) = err_shared.error.lock() {
                *g = Some(e.to_string());
            }
        },
        None,
    )?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listening_rate_fits_the_budget() {
        // short recordings: 24 kHz (or the file's own rate if lower)
        assert_eq!(listen_rate(600.0, 44_100), 24_000);
        assert_eq!(listen_rate(600.0, 16_000), 16_000);
        // long ones: within the budget, never below the analysis rate
        let r = listen_rate(3600.0, 44_100);
        assert!(r > 13_000 && r < 13_500 && r as f64 * 3600.0 <= LISTEN_MAX_SAMPLES, "{r}");
        // over 100 minutes: the analysis rate (the 2 h Oosterschelde recording)
        assert_eq!(listen_rate(7107.0, 44_100), 8000);
        assert_eq!(listen_rate(60.0, 8000), 8000);
    }

    fn tone() -> Arc<Audio> {
        let sr = 8000.0;
        Arc::new(Audio::from_samples((0..(sr * 4.0) as usize).map(|i| (0.3 * (i as f64 * 0.1).sin()) as f32).collect(), sr))
    }

    /// The callback side without a device: silence when paused, audio when
    /// playing, the position follows, and playback stops at the end point.
    #[test]
    fn callback_plays_pauses_and_stops_at_the_end() {
        let s = Shared::new(tone(), 48000.0);
        let mut out = vec![1.0f32; 960 * 2];
        s.fill(&mut out, 2);
        assert!(out.iter().all(|v| *v == 0.0), "paused: silence");
        s.playing.store(true, Ordering::Release);
        s.fill(&mut out, 2);
        assert!(out.iter().any(|v| v.abs() > 0.1));
        assert!((f64::from_bits(s.position.load(Ordering::Acquire)) - 0.02).abs() < 1e-3);
        {
            let mut r = s.renderer.lock().unwrap();
            r.seek(1.0, &s.audio.samples);
            r.set_end(Some(1.01), &s.audio.samples);
        }
        s.fill(&mut out, 2);
        assert!(!s.playing.load(Ordering::Acquire) && s.ended.load(Ordering::Acquire));
        // the 10 ms up to the end point were played, the rest is silence
        assert!(out[..800].iter().any(|v| v.abs() > 0.05));
        assert!(out[1000..].iter().all(|v| *v == 0.0));
        // a held lock (the UI changing settings) gives silence, not a wait
        s.playing.store(true, Ordering::Release);
        let g = s.renderer.lock().unwrap();
        let mut out = vec![1.0f32; 64];
        s.fill(&mut out, 1);
        assert!(out.iter().all(|v| *v == 0.0));
        drop(g);
    }

    /// With a real output device (skipped where there is none, as in CI).
    #[test]
    fn device_round_trip_when_available() {
        let Ok(p) = Player::new(tone()) else {
            eprintln!("no audio output device: skipped");
            return;
        };
        p.set_settings(ListenSettings::boost(50.0, 300.0));
        p.play_from(0.5, Some(0.7));
        std::thread::sleep(Duration::from_millis(600));
        assert!(!p.is_playing(), "stops at the end point");
        assert!(p.position() >= 0.69, "{}", p.position());
    }
}
