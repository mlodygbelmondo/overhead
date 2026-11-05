//! Audio loading: decode any supported file to mono, low-pass and decimate it.
//!
//! Aircraft Doppler tones live well below 2 kHz, so keeping the full 44.1/48 kHz
//! signal of a long recording would only waste memory. Decoding streams packets
//! through an anti-aliasing FIR decimator, so an hour of audio ends up as ~115 MB
//! at the default 8 kHz.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Default analysis sample rate (Nyquist 4 kHz).
pub const DEFAULT_TARGET_RATE: u32 = 8000;

/// Mono, decimated audio ready for analysis.
#[derive(Clone)]
pub struct Audio {
    pub samples: Vec<f32>,
    /// Effective sample rate after decimation (Hz).
    pub sample_rate: f64,
    /// Sample rate of the source file (Hz).
    pub source_rate: u32,
    pub source: PathBuf,
    /// Which decoder produced the samples ("symphonia" or "ffmpeg").
    pub decoder: &'static str,
    /// Packets that failed to decode (replaced by silence of their duration).
    pub decode_errors: usize,
    /// Silence inserted where packet timestamps jumped ahead (s). Keeps every
    /// later time on the true timeline after corrupt or missing data.
    pub gap_fill_s: f64,
    /// NaN/Inf samples replaced by zero.
    pub non_finite: usize,
    /// Decoded duration minus the duration the container declares (s), when it
    /// declares one. A large value means data went missing without timestamps
    /// to show where (e.g. corrupt raw MP3), so times after the damage are early.
    pub duration_mismatch_s: Option<f64>,
}

impl Audio {
    pub fn duration(&self) -> f64 {
        self.samples.len() as f64 / self.sample_rate
    }

    pub fn from_samples(mut samples: Vec<f32>, sample_rate: f64) -> Self {
        let non_finite = sanitize(&mut samples);
        Self {
            samples,
            sample_rate,
            source_rate: sample_rate as u32,
            source: PathBuf::new(),
            decoder: "memory",
            decode_errors: 0,
            gap_fill_s: 0.0,
            non_finite,
            duration_mismatch_s: None,
        }
    }
}

/// Replace non-finite samples with zero; returns how many there were.
fn sanitize(samples: &mut [f32]) -> usize {
    let mut n = 0;
    for s in samples.iter_mut().filter(|s| !s.is_finite()) {
        *s = 0.0;
        n += 1;
    }
    n
}

/// Decode `path` to mono and decimate to roughly `target_rate` Hz.
///
/// The decimation factor is an integer, so the effective rate is
/// `source_rate / floor(source_rate / target_rate)`.
/// Tries symphonia first; falls back to `ffmpeg` (if on PATH) for formats it
/// lacks, such as Opus or video containers.
pub fn load(path: &Path, target_rate: u32) -> Result<Audio> {
    match load_symphonia(path, target_rate) {
        Ok(audio) => Ok(audio),
        Err(symphonia_err) => load_ffmpeg(path, target_rate).map_err(|ffmpeg_err| {
            anyhow!(
                "could not decode {}:\n  symphonia: {symphonia_err:#}\n  ffmpeg: {ffmpeg_err:#}",
                path.display()
            )
        }),
    }
}

fn load_symphonia(path: &Path, target_rate: u32) -> Result<Audio> {
    load_symphonia_with(path, target_rate, decode_threads())
}

/// Threads for decoding an MP3 (the CPUs this process may use, at most 16).
fn decode_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get()).min(16)
}

/// What decoding one packet gave: its mono samples (`len` of them at `at`
/// in the chunk's buffer), a corrupt packet, or an error that stops decoding.
enum Decoded {
    Mono { at: usize, len: usize },
    Corrupt,
    Fatal,
}

/// Packets decoded by one worker: `out[i]` is packet `first + i`.
struct ChunkOut {
    first: usize,
    out: Vec<Decoded>,
    mono: Vec<f32>,
}

impl ChunkOut {
    fn samples(&self, i: usize) -> Option<&[f32]> {
        match self.out.get(i - self.first)? {
            Decoded::Mono { at, len } => Some(&self.mono[*at..*at + len]),
            _ => None,
        }
    }
    /// Packet `i` decoded to the same bits (or the same kind of failure) in both.
    fn same(&self, other: &ChunkOut, i: usize) -> bool {
        match (self.out.get(i - self.first), other.out.get(i - other.first)) {
            (Some(Decoded::Mono { .. }), Some(Decoded::Mono { .. })) => {
                let (a, b) = (self.samples(i).unwrap_or(&[]), other.samples(i).unwrap_or(&[]));
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
            }
            (Some(Decoded::Corrupt), Some(Decoded::Corrupt)) => true,
            _ => false,
        }
    }
}

/// MP3 packets per parallel chunk, packets decoded before a chunk (and
/// dropped) to bring a fresh decoder into the state of a continuous decode,
/// and packets past a chunk's end decoded by both neighbours to check that
/// they agree.
const MP3_CHUNK: usize = 1024;
/// Parallel MP3 decodes that had to start over in one piece (for the tests).
static MP3_RESTARTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Tests: treat every chunk boundary as disagreeing (exercises starting over).
#[cfg(test)]
static MP3_DISTRUST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const MP3_WARMUP: usize = 32;
const MP3_CHECK: usize = 4;

/// Mono mixdown of one decoded packet (the mean of the channels).
fn mixdown(decoded: symphonia::core::audio::AudioBufferRef<'_>, buf: &mut Option<SampleBuffer<f32>>, mono: &mut Vec<f32>) {
    let spec = *decoded.spec();
    let channels = spec.channels.count().max(1);
    let b = buf.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
    if b.capacity() < decoded.capacity() * channels {
        *b = SampleBuffer::new(decoded.capacity() as u64, spec);
    }
    b.copy_interleaved_ref(decoded);
    mono.extend(b.samples().chunks_exact(channels).map(|frame| frame.iter().sum::<f32>() / channels as f32));
}

/// Decode packets `from..to` with a fresh decoder, keeping those from `keep` on.
fn decode_chunk(
    packets: &[symphonia::core::formats::Packet],
    params: &symphonia::core::codecs::CodecParameters,
    from: usize,
    keep: usize,
    to: usize,
) -> Result<ChunkOut> {
    let mut decoder = symphonia::default::get_codecs().make(params, &DecoderOptions::default()).context("unsupported codec")?;
    let mut buf: Option<SampleBuffer<f32>> = None;
    let mut c = ChunkOut { first: keep, out: Vec::with_capacity(to - keep), mono: Vec::new() };
    for (i, packet) in packets.iter().enumerate().take(to).skip(from) {
        let r = decoder.decode(packet);
        if i < keep {
            continue;
        }
        c.out.push(match r {
            Ok(d) => {
                let at = c.mono.len();
                mixdown(d, &mut buf, &mut c.mono);
                Decoded::Mono { at, len: c.mono.len() - at }
            }
            Err(SymphoniaError::DecodeError(_)) => Decoded::Corrupt,
            Err(_) => Decoded::Fatal,
        });
    }
    Ok(c)
}

/// [`load_symphonia`] with `threads` decoding threads. An MP3 is decoded in
/// chunks of packets on several threads: a decoder's state is its bit
/// reservoir (the last few hundred bytes of main data), the IMDCT overlap
/// (the previous granule) and the synthesis filterbank (the last 16 sample
/// blocks), all set by the last frames alone, so a fresh decoder started
/// [`MP3_WARMUP`] packets early decodes the chunk to the same bits as one
/// that ran from the start. Each chunk boundary is checked: the
/// [`MP3_CHECK`] packets after it, decoded by both neighbours, must agree bit
/// for bit, else (or on any error but a corrupt packet) the file is decoded
/// again in one piece. The gap handling and the decimator run in packet order
/// as before, so the result is the same as the one-thread decode.
fn load_symphonia_with(path: &Path, target_rate: u32, threads: usize) -> Result<Audio> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        // gapless: trim encoder delay/padding so MP3/AAC start at true t = 0
        .format(&hint, mss, &FormatOptions { enable_gapless: true, ..Default::default() }, &MetadataOptions::default())
        .context("unsupported container")?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow!("no audio track"))?;
    let track_id = track.id;
    let params = track.codec_params.clone();
    let source_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| anyhow!("unknown sample rate"))?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("unsupported codec")?;

    let time_base = track.codec_params.time_base;
    let declared_frames = track.codec_params.n_frames;
    let to_samples = |ts: u64| -> Option<u64> {
        time_base.map(|tb| (ts as f64 * tb.numer as f64 / tb.denom as f64 * source_rate as f64).round() as u64)
    };
    // Jumps smaller than this are encoder delay or rounding, not lost data.
    let gap_threshold = source_rate as u64 / 10;

    let mut decimator = Decimator::new(source_rate, target_rate);
    let mut mono = Vec::new();
    let mut out = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut produced: u64 = 0;
    let mut origin: Option<u64> = None;
    let (mut decode_errors, mut gap_samples) = (0usize, 0u64);
    // the timeline before a packet: a jump in its timestamp is filled with silence
    let before = |packet: &symphonia::core::formats::Packet,
                  origin: &mut Option<u64>,
                  produced: &mut u64,
                  gap_samples: &mut u64,
                  out: &mut Vec<f32>,
                  d: &mut Decimator| {
        if let Some(pos) = to_samples(packet.ts()) {
            let expected = pos.saturating_sub(*origin.get_or_insert(pos));
            if expected > *produced + gap_threshold {
                let gap = expected - *produced;
                d.process_silence(gap, out);
                *produced += gap;
                *gap_samples += gap;
            }
        }
    };
    let parallel = threads > 1 && params.codec == symphonia::core::codecs::CODEC_TYPE_MP3;
    let mut packets: Vec<symphonia::core::formats::Packet> = Vec::new();
    let mut done = false;
    if parallel {
        loop {
            match format.next_packet() {
                Ok(p) if p.track_id() == track_id => packets.push(p),
                Ok(_) => {}
                Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(SymphoniaError::ResetRequired) => bail!("stream reset required"),
                Err(e) => return Err(e.into()),
            }
        }
        let n = packets.len();
        let starts: Vec<usize> = (0..n).step_by(MP3_CHUNK).collect();
        let mut prev: Option<ChunkOut> = None;
        let mut ok = true;
        'waves: for wave in starts.chunks(2 * threads) {
            let next = std::sync::atomic::AtomicUsize::new(0);
            let mut chunks: Vec<Option<Result<ChunkOut>>> = (0..wave.len()).map(|_| None).collect();
            std::thread::scope(|sc| {
                let workers: Vec<_> = (0..threads.min(wave.len()))
                    .map(|_| {
                        sc.spawn(|| {
                            let mut mine = Vec::new();
                            loop {
                                let j = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                let Some(&s) = wave.get(j) else { break };
                                let from = s.saturating_sub(MP3_WARMUP);
                                let to = (s + MP3_CHUNK + MP3_CHECK).min(n);
                                mine.push((j, decode_chunk(&packets, &params, from, s, to)));
                            }
                            mine
                        })
                    })
                    .collect();
                for w in workers {
                    for (j, c) in w.join().expect("decode thread") {
                        chunks[j] = Some(c);
                    }
                }
            });
            for c in chunks {
                let c = c.expect("every chunk decoded")?;
                let s = c.first;
                let end = (s + MP3_CHUNK).min(n);
                // the packets after the previous chunk's end, decoded by both
                #[cfg(test)]
                let distrust = MP3_DISTRUST.load(std::sync::atomic::Ordering::Relaxed);
                #[cfg(not(test))]
                let distrust = false;
                if let Some(p) = &prev
                    && (distrust || !(s..(s + MP3_CHECK).min(n)).all(|i| p.same(&c, i)))
                {
                    ok = false;
                    break 'waves;
                }
                for (i, packet) in (s..end).zip(&packets[s..end]) {
                    before(packet, &mut origin, &mut produced, &mut gap_samples, &mut out, &mut decimator);
                    match &c.out[i - s] {
                        Decoded::Mono { .. } => {
                            let m = c.samples(i).unwrap_or(&[]);
                            produced += m.len() as u64;
                            decimator.process(m, &mut out);
                        }
                        Decoded::Corrupt => {
                            decode_errors += 1;
                            let dur = to_samples(packet.dur()).unwrap_or(0);
                            decimator.process_silence(dur, &mut out);
                            produced += dur;
                        }
                        Decoded::Fatal => {
                            ok = false;
                            break 'waves;
                        }
                    }
                }
                prev = Some(c);
            }
        }
        if ok {
            done = true;
        } else {
            // start over in one piece
            MP3_RESTARTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            decimator = Decimator::new(source_rate, target_rate);
            out.clear();
            (produced, origin, decode_errors, gap_samples) = (0, None, 0, 0);
        }
    }
    let mut packets = packets.into_iter();
    // the sequential decode, unless the parallel one went through
    if !done {
        loop {
            let packet = if parallel {
                match packets.next() {
                    Some(p) => p,
                    None => break,
                }
            } else {
                match format.next_packet() {
                    Ok(p) => p,
                    Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    // a chained stream would need a new decoder; let ffmpeg handle it
                    Err(SymphoniaError::ResetRequired) => bail!("stream reset required"),
                    Err(e) => return Err(e.into()),
                }
            };
            if packet.track_id() != track_id {
                continue;
            }
            before(&packet, &mut origin, &mut produced, &mut gap_samples, &mut out, &mut decimator);
            let decoded = match decoder.decode(&packet) {
                Ok(d) => d,
                // corrupt packet: keep the timeline by substituting silence
                Err(SymphoniaError::DecodeError(_)) => {
                    decode_errors += 1;
                    let dur = to_samples(packet.dur()).unwrap_or(0);
                    decimator.process_silence(dur, &mut out);
                    produced += dur;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            mono.clear();
            mixdown(decoded, &mut sample_buf, &mut mono);
            produced += mono.len() as u64;
            decimator.process(&mono, &mut out);
        }
    }
    decimator.finish(&mut out);
    if out.is_empty() {
        bail!("no samples decoded");
    }
    let non_finite = sanitize(&mut out);
    Ok(Audio {
        samples: out,
        sample_rate: decimator.output_rate(),
        source_rate,
        source: path.to_path_buf(),
        decoder: "symphonia",
        decode_errors,
        gap_fill_s: gap_samples as f64 / source_rate as f64,
        non_finite,
        // n_frames is in time-base units (ms for mkv), not always samples
        duration_mismatch_s: declared_frames.and_then(to_samples).map(|n| (produced as f64 - n as f64) / source_rate as f64),
    })
}

fn load_ffmpeg(path: &Path, target_rate: u32) -> Result<Audio> {
    // ffmpeg resamples to 48 kHz mono; our own decimator then does the rest, so
    // both decode paths share identical anti-aliasing.
    const FFMPEG_RATE: u32 = 48_000;
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        // aresample async: pad timestamp gaps with silence so later times stay true
        .args(["-vn", "-ac", "1", "-af", "aresample=async=1:first_pts=0", "-ar", &FFMPEG_RATE.to_string(), "-f", "f32le", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("ffmpeg not found on PATH")?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    // Drain stderr concurrently: a full stderr pipe would block ffmpeg while
    // we block on stdout. Keep only the tail for the error message.
    let mut stderr = child.stderr.take().expect("piped stderr");
    let stderr_tail = std::thread::spawn(move || {
        let mut tail: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut chunk) {
            if n == 0 {
                break;
            }
            tail.extend_from_slice(&chunk[..n]);
            if tail.len() > 8192 {
                tail.drain(..tail.len() - 8192);
            }
        }
        String::from_utf8_lossy(&tail).trim().to_string()
    });
    let mut decimator = Decimator::new(FFMPEG_RATE, target_rate);
    let mut out = Vec::new();
    let mut bytes = vec![0u8; 1 << 16];
    let mut carry: Vec<u8> = Vec::new();
    let mut frame = Vec::new();
    loop {
        let n = match stdout.read(&mut bytes) {
            Ok(n) => n,
            Err(e) => {
                // don't leave ffmpeg running behind a failed read
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.into());
            }
        };
        if n == 0 {
            break;
        }
        carry.extend_from_slice(&bytes[..n]);
        let whole = carry.len() / 4 * 4;
        frame.clear();
        frame.extend(
            carry[..whole]
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        );
        carry.drain(..whole);
        decimator.process(&frame, &mut out);
    }
    decimator.finish(&mut out);
    let status = child.wait()?;
    let errors = stderr_tail.join().unwrap_or_default();
    if !status.success() || out.is_empty() {
        bail!("ffmpeg failed: {errors}");
    }
    let non_finite = sanitize(&mut out);
    Ok(Audio {
        samples: out,
        sample_rate: decimator.output_rate(),
        source_rate: FFMPEG_RATE,
        source: path.to_path_buf(),
        decoder: "ffmpeg",
        // ffmpeg reports problems only as free text (even for clean files), so
        // nothing reliable to count here
        decode_errors: 0,
        gap_fill_s: 0.0,
        non_finite,
        duration_mismatch_s: None,
    })
}

/// Streaming integer-factor decimator with a windowed-sinc low-pass FIR.
pub struct Decimator {
    taps: Vec<f32>,
    factor: usize,
    input_rate: u32,
    buf: Vec<f32>,
    /// Index in `buf` of the newest sample of the next output.
    next: usize,
    /// Outputs still to drop at the start: the FIR's group delay, so output
    /// sample k is aligned with input time k / output_rate.
    skip: usize,
}

impl Decimator {
    pub fn new(input_rate: u32, target_rate: u32) -> Self {
        let factor = (input_rate / target_rate.max(1)).max(1) as usize;
        let taps = if factor == 1 { vec![1.0] } else { lowpass_taps(factor) };
        let history = taps.len() - 1;
        // symmetric FIR: delay is (n-1)/2 input samples = 8 * factor
        let skip = history / 2 / factor;
        Self {
            factor,
            input_rate,
            buf: vec![0.0; history],
            next: history,
            taps,
            skip,
        }
    }

    pub fn output_rate(&self) -> f64 {
        self.input_rate as f64 / self.factor as f64
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.buf.extend_from_slice(input);
        let n_taps = self.taps.len();
        while self.next < self.buf.len() {
            // LANES outputs at once while they are all in the buffer: each
            // is the same sum in the same order as one output below (from
            // -0.0, as `Iterator::sum`), only LANES independent sums are
            // interleaved so they do not wait on each other's additions
            if self.next + (LANES - 1) * self.factor < self.buf.len() {
                let y = fir_lanes(&self.buf, self.next + 1 - n_taps, self.factor, &self.taps);
                for v in y {
                    if self.skip > 0 {
                        self.skip -= 1;
                    } else {
                        out.push(v);
                    }
                }
                self.next += LANES * self.factor;
                continue;
            }
            let window = &self.buf[self.next + 1 - n_taps..=self.next];
            // taps are symmetric, so the convolution needs no reversal.
            let y: f32 = window.iter().zip(&self.taps).map(|(x, h)| x * h).sum();
            if self.skip > 0 {
                self.skip -= 1;
            } else {
                out.push(y);
            }
            self.next += self.factor;
        }
        let drain = self.next + 1 - n_taps;
        let drain = drain.min(self.buf.len());
        self.buf.drain(..drain);
        self.next -= drain;
    }
}

/// Outputs [`Decimator::process`] computes together.
const LANES: usize = 8;

/// `LANES` FIR outputs: output `i` is `Σ_t buf[start + i·step + t]·taps[t]`,
/// summed in `t` order from -0.0 (bit for bit `Iterator::sum` of the
/// products; no fused multiply-add).
fn fir_lanes(buf: &[f32], start: usize, step: usize, taps: &[f32]) -> [f32; LANES] {
    let n = taps.len();
    let w: [&[f32]; LANES] = std::array::from_fn(|i| &buf[start + i * step..start + i * step + n]);
    let mut acc = [-0.0f32; LANES];
    for (t, &h) in taps.iter().enumerate() {
        for (a, wi) in acc.iter_mut().zip(&w) {
            *a += wi[t] * h;
        }
    }
    acc
}

impl Decimator {
    /// Feed `n` zero samples (gaps, undecodable packets).
    pub fn process_silence(&mut self, mut n: u64, out: &mut Vec<f32>) {
        let zeros = [0.0f32; 4096];
        while n > 0 {
            let k = n.min(zeros.len() as u64) as usize;
            self.process(&zeros[..k], out);
            n -= k as u64;
        }
    }

    /// Flush the filter's group delay so the output covers the whole input.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        let delay = (self.taps.len() - 1) / 2;
        self.process_silence(delay as u64, out);
    }
}

/// Blackman-windowed sinc low-pass with cutoff at 90% of the output Nyquist.
fn lowpass_taps(factor: usize) -> Vec<f32> {
    let n = 16 * factor + 1;
    let fc = 0.45 / factor as f64; // cycles per input sample
    let mid = (n / 2) as f64;
    let mut taps: Vec<f64> = (0..n)
        .map(|i| {
            let x = i as f64 - mid;
            let sinc = if x == 0.0 {
                2.0 * fc
            } else {
                (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x)
            };
            let w = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos()
                + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
            sinc * w
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    taps.iter_mut().for_each(|t| *t /= sum);
    taps.into_iter().map(|t| t as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f64, rate: u32, secs: f64) -> Vec<f32> {
        (0..(rate as f64 * secs) as usize)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin() as f32)
            .collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    /// The single-output FIR of the original decimator, for comparison.
    fn decimate_one_by_one(input_rate: u32, target_rate: u32, chunks: &[&[f32]]) -> Vec<f32> {
        let mut d = Decimator::new(input_rate, target_rate);
        let mut out = Vec::new();
        let n_taps = d.taps.len();
        let feed = |d: &mut Decimator, input: &[f32], out: &mut Vec<f32>| {
            d.buf.extend_from_slice(input);
            while d.next < d.buf.len() {
                let window = &d.buf[d.next + 1 - n_taps..=d.next];
                let y: f32 = window.iter().zip(&d.taps).map(|(x, h)| x * h).sum();
                if d.skip > 0 {
                    d.skip -= 1;
                } else {
                    out.push(y);
                }
                d.next += d.factor;
            }
            let drain = (d.next + 1 - n_taps).min(d.buf.len());
            d.buf.drain(..drain);
            d.next -= drain;
        };
        for c in chunks {
            feed(&mut d, c, &mut out);
        }
        feed(&mut d, &vec![0.0; (n_taps - 1) / 2], &mut out);
        out
    }

    /// Computing several outputs together gives the same bits as one by one,
    /// for every rate pair, chunking, signed zeros and NaN.
    #[test]
    fn decimator_lanes_are_bit_identical() {
        let mut s = 0x2545f4914f6cdd1du64;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        for (case, (rate, target)) in
            [(48_000, 8000), (44_100, 8000), (22_050, 8000), (8000, 8000), (16_000, 8000), (96_000, 8000), (11_025, 8000)].into_iter().enumerate()
        {
            let mut x: Vec<f32> = (0..30_000).map(|_| (rnd() * 2.0 - 1.0) as f32).collect();
            // silence (signed zeros) and a NaN
            for v in &mut x[1000..1500] {
                *v = if case % 2 == 0 { -0.0 } else { 0.0 };
            }
            if case == 3 {
                x[20_000] = f32::NAN;
            }
            let mut sizes = Vec::new();
            let mut at = 0;
            while at < x.len() {
                let k = (1 + (rnd() * 3000.0) as usize).min(x.len() - at);
                sizes.push(&x[at..at + k]);
                at += k;
            }
            let want = decimate_one_by_one(rate, target, &sizes);
            let mut d = Decimator::new(rate, target);
            let mut got = Vec::new();
            for c in &sizes {
                d.process(c, &mut got);
            }
            d.finish(&mut got);
            assert_eq!(want.len(), got.len(), "{rate}");
            for (i, (a, b)) in want.iter().zip(&got).enumerate() {
                assert_eq!(a.to_bits(), b.to_bits(), "{rate} sample {i}: {a} vs {b}");
            }
        }
    }

    /// An MP3 made by ffmpeg (None without ffmpeg): a gliding tone in noise,
    /// stereo, at a low bit rate so frames lean on the bit reservoir.
    fn mp3(dir: &Path, name: &str, secs: f64, args: &[&str]) -> Option<PathBuf> {
        let rate = 48_000u32;
        let n = (secs * rate as f64) as usize;
        let mut s = 0x2545f4914f6cdd1du64;
        let mut pcm = Vec::with_capacity(n * 4);
        for i in 0..n {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let t = i as f64 / rate as f64;
            let tone = (2.0 * std::f64::consts::PI * (150.0 * t + 2.0 * t * t)).sin() * 0.3;
            let noise = ((s >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 0.2;
            for ch in [tone + noise, 0.5 * tone - noise] {
                pcm.extend_from_slice(&((ch * 32767.0) as i16).to_le_bytes());
            }
        }
        let raw = dir.join(format!("{name}.raw"));
        std::fs::write(&raw, &pcm).ok()?;
        let out = dir.join(format!("{name}.mp3"));
        let ok = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "s16le", "-ar", "48000", "-ac", "2", "-i"])
            .arg(&raw)
            .args(args)
            .arg(&out)
            .status()
            .ok()?
            .success();
        ok.then_some(out)
    }

    fn same_audio(a: &Audio, b: &Audio) -> bool {
        a.samples.len() == b.samples.len()
            && a.samples.iter().zip(&b.samples).all(|(x, y)| x.to_bits() == y.to_bits())
            && a.decode_errors == b.decode_errors
            && a.gap_fill_s == b.gap_fill_s
            && a.duration_mismatch_s == b.duration_mismatch_s
    }

    /// An MP3 decoded in parallel chunks gives the bits of the one-thread
    /// decode, without starting over; a damaged one too (starting over if a
    /// chunk boundary does not agree).
    #[test]
    fn parallel_mp3_decode_is_bit_identical() {
        let dir = std::env::temp_dir().join(format!("overhead-mp3-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files = [
            mp3(&dir, "cbr", 75.0, &["-b:a", "32k"]),
            mp3(&dir, "vbr", 75.0, &["-q:a", "8"]),
            mp3(&dir, "mono", 40.0, &["-ac", "1", "-b:a", "24k"]),
        ];
        if files.iter().any(|f| f.is_none()) {
            eprintln!("ffmpeg not available: parallel MP3 decode not tested");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let files: Vec<PathBuf> = files.into_iter().flatten().collect();
        let restarts = MP3_RESTARTS.load(std::sync::atomic::Ordering::Relaxed);
        for f in &files {
            let one = load_symphonia_with(f, 8000, 1).unwrap();
            for threads in [2, 3, 8] {
                let par = load_symphonia_with(f, 8000, threads).unwrap();
                assert!(same_audio(&one, &par), "{} with {threads} threads", f.display());
            }
            assert!(one.duration() > 30.0);
        }
        assert_eq!(MP3_RESTARTS.load(std::sync::atomic::Ordering::Relaxed), restarts, "a clean file needed no restart");
        // damage: garbage over a few frames inside chunks and across a chunk boundary
        let mut bytes = std::fs::read(&files[0]).unwrap();
        let len = bytes.len();
        for at in [len / 7, len / 3, len / 2 + 17, (len as f64 * 0.81) as usize] {
            for (k, b) in bytes[at..at + 700].iter_mut().enumerate() {
                *b = (k as u8).wrapping_mul(37) ^ 0x5a;
            }
        }
        let bad = dir.join("damaged.mp3");
        std::fs::write(&bad, &bytes).unwrap();
        match (load_symphonia_with(&bad, 8000, 1), load_symphonia_with(&bad, 8000, 4)) {
            (Ok(one), Ok(par)) => assert!(same_audio(&one, &par), "damaged file"),
            (Err(_), Err(_)) => {}
            (a, b) => panic!("damaged file: one thread {:?}, parallel {:?}", a.is_ok(), b.is_ok()),
        }
        // a boundary that does not agree: decoded again in one piece, same bits
        let one = load_symphonia_with(&files[1], 8000, 1).unwrap();
        let before = MP3_RESTARTS.load(std::sync::atomic::Ordering::Relaxed);
        MP3_DISTRUST.store(true, std::sync::atomic::Ordering::Relaxed);
        let again = load_symphonia_with(&files[1], 8000, 4);
        MP3_DISTRUST.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(same_audio(&one, &again.unwrap()), "after starting over");
        assert!(MP3_RESTARTS.load(std::sync::atomic::Ordering::Relaxed) > before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decimator_has_no_time_offset() {
        // a click at input sample 48_000 (t = 1 s) must peak at output 8000
        let mut input = vec![0.0f32; 96_000];
        input[48_000] = 1.0;
        let mut out = Vec::new();
        let mut d = Decimator::new(48_000, 8000);
        d.process(&input, &mut out);
        d.finish(&mut out);
        assert_eq!(out.len(), 16_000);
        let peak = out.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        assert_eq!(peak, 8000);
    }

    #[test]
    fn decimator_keeps_passband_and_rejects_alias() {
        let rate = 48_000;
        let mut pass_out = Vec::new();
        let mut d = Decimator::new(rate, 8000);
        // feed in odd-sized chunks to exercise the streaming state
        for chunk in tone(200.0, rate, 2.0).chunks(777) {
            d.process(chunk, &mut pass_out);
        }
        d.finish(&mut pass_out);
        assert_eq!(d.output_rate(), 8000.0);
        assert!((pass_out.len() as i64 - 16_000).abs() <= 1);
        let pass = rms(&pass_out[2000..]);
        assert!((pass - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.01, "pass rms {pass}");

        // 7 kHz would alias to 1 kHz without filtering.
        let mut stop_out = Vec::new();
        let mut d = Decimator::new(rate, 8000);
        d.process(&tone(7000.0, rate, 2.0), &mut stop_out);
        let stop = rms(&stop_out[2000..]);
        assert!(stop < 1e-3, "stop rms {stop}");
    }
}
