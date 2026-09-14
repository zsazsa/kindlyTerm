//! Voice input: microphone → voice activity detection → on-device speech
//! recognition (NVIDIA Parakeet-TDT through sherpa-onnx) → text typed into
//! the focused terminal.
//!
//! Everything heavy runs on one worker thread. The UI thread sends it
//! commands, the audio callback sends it samples, and it reports back with
//! [`VoiceMsg`] through a channel plus a wake-up on the winit event loop,
//! the same pattern the control socket uses.
//!
//! Text can only be committed once an utterance ends: bytes written to a
//! shell cannot be taken back. So the perceived delay is the endpoint
//! (silence after speech, `endpoint_ms`) plus inference, which on a modern
//! CPU is a few hundred milliseconds for a sentence and far less on a GPU.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::Arc;
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::config::VoiceConfig;

const SAMPLE_RATE: u32 = 16_000;
/// Silero VAD works on 512-sample windows at 16 kHz (32 ms).
const VAD_WINDOW: usize = 512;
/// Longest single utterance we hand to the recognizer, in seconds.
const MAX_UTTERANCE_S: f32 = 30.0;

/// What the worker tells the UI.
#[derive(Debug, Clone)]
pub enum VoiceMsg {
    /// Models loaded; the microphone is not open yet.
    Ready,
    /// Microphone open and audio flowing.
    Listening,
    /// Input level in 0..1 (log scale), a few times a second while listening.
    Level(f32),
    /// An utterance ended; the recognizer is working on it.
    Transcribing,
    /// Recognized text for one utterance.
    Text(String),
    /// Microphone closed.
    Stopped,
    /// Something failed; the engine is unusable until re-created.
    Error(String),
}

/// UI-side view of the worker's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceState {
    Loading,
    Idle,
    Listening,
    Transcribing,
    Error,
}

enum Cmd {
    Start,
    /// Stop listening; `flush` commits whatever speech is in progress.
    Stop { flush: bool },
}

enum Ev {
    Cmd(Cmd),
    /// Mono 16 kHz samples from the microphone.
    Audio(Vec<f32>),
    /// The audio stream itself failed.
    AudioError(String),
}

/// Handle owned by the UI thread.
pub struct Voice {
    tx: Sender<Ev>,
    rx: Receiver<VoiceMsg>,
    pub state: VoiceState,
    pub level: f32,
    /// Start as soon as the models are loaded (the key was pressed while loading).
    pub start_when_ready: bool,
    pub last_error: Option<String>,
}

impl Voice {
    /// Spawn the worker. It loads the models first and reports `Ready` or
    /// `Error`. `wake` is called after every message so the UI drains them.
    pub fn spawn(cfg: VoiceConfig, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (tx, worker_rx) = channel::<Ev>();
        let (msg_tx, rx) = channel::<VoiceMsg>();
        let wake = Arc::new(wake);
        let audio_tx = tx.clone();
        let _ = std::thread::Builder::new().name("voice".into()).spawn(move || {
            let report = move |m: VoiceMsg| {
                let _ = msg_tx.send(m);
                wake();
            };
            match Engine::load(&cfg) {
                Ok(engine) => {
                    report(VoiceMsg::Ready);
                    engine.run(worker_rx, audio_tx, report);
                }
                Err(e) => report(VoiceMsg::Error(e)),
            }
        });
        Self { tx, rx, state: VoiceState::Loading, level: 0.0, start_when_ready: false, last_error: None }
    }

    pub fn start(&self) {
        let _ = self.tx.send(Ev::Cmd(Cmd::Start));
    }

    pub fn stop(&self, flush: bool) {
        let _ = self.tx.send(Ev::Cmd(Cmd::Stop { flush }));
    }

    /// Take everything the worker reported, updating `state` and `level`.
    pub fn drain(&mut self) -> Vec<VoiceMsg> {
        let mut out = Vec::new();
        while let Ok(m) = self.rx.try_recv() {
            match &m {
                VoiceMsg::Ready | VoiceMsg::Stopped => {
                    self.state = VoiceState::Idle;
                    self.level = 0.0;
                }
                VoiceMsg::Listening => self.state = VoiceState::Listening,
                VoiceMsg::Level(l) => self.level = *l,
                VoiceMsg::Transcribing => self.state = VoiceState::Transcribing,
                VoiceMsg::Text(_) => {}
                VoiceMsg::Error(e) => {
                    self.state = VoiceState::Error;
                    self.last_error = Some(e.clone());
                }
            }
            out.push(m);
        }
        out
    }
}

/// Where the models live unless the config says otherwise.
pub fn default_model_dir() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("kindlyterm").join("voice")
}

/// The Parakeet files (encoder, decoder, joiner, tokens) found under `dir`.
struct ModelFiles {
    encoder: PathBuf,
    decoder: PathBuf,
    joiner: PathBuf,
    tokens: PathBuf,
    vad: PathBuf,
}

fn find_models(dir: &Path, prefer_int8: bool) -> Result<ModelFiles, String> {
    let missing = || {
        format!(
            "voice models not found in {} (run voice-models.sh to download Parakeet-TDT and the Silero VAD)",
            dir.display()
        )
    };
    let mut vad = None;
    let mut model_dir = None;
    let mut dirs_to_scan = vec![dir.to_path_buf()];
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs_to_scan.push(p);
            }
        }
    }
    for d in &dirs_to_scan {
        if vad.is_none() && d.join("silero_vad.onnx").is_file() {
            vad = Some(d.join("silero_vad.onnx"));
        }
        if model_dir.is_none() && d.join("tokens.txt").is_file() {
            model_dir = Some(d.clone());
        }
    }
    let (Some(vad), Some(md)) = (vad, model_dir) else { return Err(missing()) };
    let pick = |stem: &str| -> Option<PathBuf> {
        let mut candidates: Vec<PathBuf> = std::fs::read_dir(&md)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                n.starts_with(stem) && n.ends_with(".onnx")
            })
            .collect();
        candidates.sort();
        let int8 = candidates.iter().find(|p| p.to_string_lossy().contains("int8")).cloned();
        let full = candidates.iter().find(|p| !p.to_string_lossy().contains("int8")).cloned();
        if prefer_int8 { int8.or(full) } else { full.or(int8) }
    };
    let (Some(encoder), Some(decoder), Some(joiner)) = (pick("encoder"), pick("decoder"), pick("joiner")) else {
        return Err(missing());
    };
    Ok(ModelFiles { encoder, decoder, joiner, tokens: md.join("tokens.txt"), vad })
}

struct Engine {
    recognizer: sherpa_rs::transducer::TransducerRecognizer,
    vad: sherpa_rs::silero_vad::SileroVad,
}

impl Engine {
    fn load(cfg: &VoiceConfig) -> Result<Self, String> {
        let dir = cfg.model_dir.as_deref().map(PathBuf::from).unwrap_or_else(default_model_dir);
        let provider = match cfg.device.as_str() {
            "cuda" if cfg!(feature = "voice-cuda") => "cuda",
            "cuda" => {
                log::warn!("voice.device = \"cuda\" needs a build with --features voice-cuda; using the CPU");
                "cpu"
            }
            _ => "cpu",
        };
        let files = find_models(&dir, provider == "cpu")?;
        let threads = cfg.threads.clamp(1, 64) as i32;
        let t0 = Instant::now();
        let recognizer = sherpa_rs::transducer::TransducerRecognizer::new(sherpa_rs::transducer::TransducerConfig {
            encoder: files.encoder.to_string_lossy().into_owned(),
            decoder: files.decoder.to_string_lossy().into_owned(),
            joiner: files.joiner.to_string_lossy().into_owned(),
            tokens: files.tokens.to_string_lossy().into_owned(),
            model_type: "nemo_transducer".into(),
            num_threads: threads,
            sample_rate: SAMPLE_RATE as i32,
            feature_dim: 80,
            decoding_method: "greedy_search".into(),
            provider: Some(provider.into()),
            ..Default::default()
        })
        .map_err(|e| format!("could not load the speech model: {e}"))?;
        let vad = sherpa_rs::silero_vad::SileroVad::new(
            sherpa_rs::silero_vad::SileroVadConfig {
                model: files.vad.to_string_lossy().into_owned(),
                min_silence_duration: (cfg.endpoint_ms.max(50) as f32) / 1000.0,
                min_speech_duration: 0.2,
                max_speech_duration: MAX_UTTERANCE_S,
                threshold: 0.5,
                sample_rate: SAMPLE_RATE,
                window_size: VAD_WINDOW as i32,
                provider: Some("cpu".into()),
                num_threads: Some(1),
                debug: false,
            },
            MAX_UTTERANCE_S + 5.0,
        )
        .map_err(|e| format!("could not load the voice activity model: {e}"))?;
        log::info!("voice: models loaded from {} in {:.1}s ({provider})", dir.display(), t0.elapsed().as_secs_f32());
        Ok(Self { recognizer, vad })
    }

    fn run(mut self, rx: Receiver<Ev>, audio_tx: Sender<Ev>, report: impl Fn(VoiceMsg)) {
        let mut stream: Option<cpal::Stream> = None;
        let mut pending: Vec<f32> = Vec::with_capacity(VAD_WINDOW * 4);
        let mut level_at = Instant::now();
        while let Ok(ev) = rx.recv() {
            match ev {
                Ev::Cmd(Cmd::Start) => {
                    if stream.is_some() {
                        continue;
                    }
                    match open_microphone(audio_tx.clone()) {
                        Ok(s) => {
                            stream = Some(s);
                            pending.clear();
                            self.vad.clear();
                            report(VoiceMsg::Listening);
                        }
                        Err(e) => report(VoiceMsg::Error(e)),
                    }
                }
                Ev::Cmd(Cmd::Stop { flush }) => {
                    let was_open = stream.take().is_some();
                    if was_open && flush {
                        // Feed the tail that did not fill a whole window, then
                        // close the utterance in progress.
                        if !pending.is_empty() {
                            pending.resize(VAD_WINDOW, 0.0);
                            self.vad.accept_waveform(std::mem::take(&mut pending));
                        }
                        self.vad.flush();
                        self.emit_segments(&report);
                    }
                    pending.clear();
                    self.vad.clear();
                    report(VoiceMsg::Stopped);
                }
                Ev::Audio(samples) => {
                    if stream.is_none() {
                        continue;
                    }
                    if level_at.elapsed().as_millis() >= 60 {
                        level_at = Instant::now();
                        report(VoiceMsg::Level(level_of(&samples)));
                    }
                    pending.extend_from_slice(&samples);
                    while pending.len() >= VAD_WINDOW {
                        let window: Vec<f32> = pending.drain(..VAD_WINDOW).collect();
                        self.vad.accept_waveform(window);
                    }
                    self.emit_segments(&report);
                }
                Ev::AudioError(e) => {
                    stream = None;
                    report(VoiceMsg::Error(format!("microphone stopped: {e}")));
                }
            }
        }
    }

    /// Transcribe every finished speech segment the VAD holds.
    fn emit_segments(&mut self, report: &impl Fn(VoiceMsg)) {
        while !self.vad.is_empty() {
            let seg = self.vad.front();
            self.vad.pop();
            if seg.samples.len() < (SAMPLE_RATE as usize) / 10 {
                continue;
            }
            report(VoiceMsg::Transcribing);
            let t0 = Instant::now();
            let text = self.recognizer.transcribe(SAMPLE_RATE, &seg.samples);
            let text = text.trim().to_string();
            log::info!(
                "voice: {:.2}s of speech → {:?} in {} ms",
                seg.samples.len() as f32 / SAMPLE_RATE as f32,
                text,
                t0.elapsed().as_millis()
            );
            if !text.is_empty() {
                report(VoiceMsg::Text(text));
            }
            report(VoiceMsg::Listening);
        }
    }
}

/// What a dictated utterance asks for besides plain text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceAction {
    /// Press a key: the bytes to send.
    Key(&'static [u8]),
    NextTab,
    PrevTab,
    /// "switch to <query>": focus the best-matching card or tab.
    Navigate(String),
}

/// The action for `text`, if it is exactly a configured command phrase or
/// starts with a navigation prefix. Case, punctuation and extra spaces the
/// recognizer adds are ignored ("Enter." matches "enter"); a command word
/// inside a longer sentence does not match, so "press enter to continue"
/// is typed as text.
pub fn command_action(text: &str, cfg: &VoiceConfig) -> Option<VoiceAction> {
    let said = normalize_phrase(text);
    if said.is_empty() {
        return None;
    }
    if let Some(key) = cfg.commands.iter().find(|(phrase, _)| normalize_phrase(phrase) == said).map(|(_, k)| k.as_str()) {
        return key_action(key);
    }
    for prefix in &cfg.navigate {
        let prefix = normalize_phrase(prefix);
        if prefix.is_empty() {
            continue;
        }
        if let Some(rest) = said.strip_prefix(prefix.as_str())
            && let Some(query) = rest.strip_prefix(' ')
            && !query.trim().is_empty()
        {
            return Some(VoiceAction::Navigate(query.trim().to_string()));
        }
    }
    None
}

/// Lowercase letters and digits only, single spaces between words.
fn normalize_phrase(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.extend(ch.to_lowercase());
        } else if ch.is_whitespace() || ch.is_ascii_punctuation() {
            space = true;
        }
    }
    out
}

/// What a key name from the config does.
fn key_action(key: &str) -> Option<VoiceAction> {
    Some(match key.trim().to_ascii_lowercase().as_str() {
        "enter" | "return" => VoiceAction::Key(b"\r"),
        "tab" => VoiceAction::Key(b"\t"),
        "escape" | "esc" => VoiceAction::Key(b"\x1b"),
        "backspace" => VoiceAction::Key(b"\x7f"),
        "space" => VoiceAction::Key(b" "),
        "next-tab" => VoiceAction::NextTab,
        "previous-tab" | "prev-tab" => VoiceAction::PrevTab,
        other => {
            log::warn!("voice: unknown key {other:?} in [voice].commands");
            return None;
        }
    })
}

/// Input level on a log scale: silence → 0, a loud voice → 1.
fn level_of(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    let db = 20.0 * rms.max(1e-6).log10();
    ((db + 50.0) / 45.0).clamp(0.0, 1.0)
}

/// Open the default input device and stream mono 16 kHz samples to `tx`.
/// The stream is returned so the caller decides how long it lives.
fn open_microphone(tx: Sender<Ev>) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let device = host.default_input_device().ok_or_else(|| "no microphone found".to_string())?;
    let name = device.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "microphone".into());
    let supported = device.default_input_config().map_err(|e| format!("{name}: {e}"))?;
    let channels = supported.channels() as usize;
    let in_rate = supported.sample_rate();
    let config: cpal::StreamConfig = supported.clone().into();
    let err_tx = tx.clone();
    let err_fn = move |e: cpal::Error| {
        let _ = err_tx.send(Ev::AudioError(e.to_string()));
    };
    let mut conv = Downmix::new(channels, in_rate, SAMPLE_RATE);
    log::info!("voice: capturing from {name} at {in_rate} Hz, {channels} ch");
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config.clone(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let _ = tx.send(Ev::Audio(conv.push(data.iter().copied())));
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            config.clone(),
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let _ = tx.send(Ev::Audio(conv.push(data.iter().map(|&s| s as f32 / 32768.0))));
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            config,
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                let _ = tx.send(Ev::Audio(conv.push(data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0))));
            },
            err_fn,
            None,
        ),
        other => return Err(format!("{name}: unsupported sample format {other:?}")),
    }
    .map_err(|e| format!("{name}: {e}"))?;
    stream.play().map_err(|e| format!("{name}: {e}"))?;
    Ok(stream)
}

/// Interleaved multi-channel audio at one rate → mono at another, by
/// averaging channels and linear interpolation. Plenty for speech models,
/// which resample to 16 kHz internally anyway.
struct Downmix {
    channels: usize,
    step: f64,
    /// Position of the next output sample in input frames, where frame 0
    /// is `last` (the previous chunk's final sample) and frame 1 is the
    /// first sample of the current chunk.
    pos: f64,
    last: f32,
}

impl Downmix {
    fn new(channels: usize, in_rate: u32, out_rate: u32) -> Self {
        Self { channels: channels.max(1), step: in_rate as f64 / out_rate as f64, pos: 1.0, last: 0.0 }
    }

    fn push(&mut self, samples: impl Iterator<Item = f32>) -> Vec<f32> {
        let mut mono: Vec<f32> = Vec::new();
        let mut acc = 0.0f32;
        let mut n = 0;
        for s in samples {
            acc += s;
            n += 1;
            if n == self.channels {
                mono.push(acc / self.channels as f32);
                acc = 0.0;
                n = 0;
            }
        }
        // Frames available for interpolation: `last` followed by `mono`.
        let mut out = Vec::with_capacity((mono.len() as f64 / self.step) as usize + 2);
        while self.pos < mono.len() as f64 {
            let i = self.pos.floor();
            let frac = (self.pos - i) as f32;
            let i = i as usize;
            let a = if i == 0 { self.last } else { mono[i - 1] };
            let b = mono[i];
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        if let Some(&l) = mono.last() {
            self.last = l;
        }
        self.pos -= mono.len() as f64;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_channels_and_keeps_rate() {
        let mut d = Downmix::new(2, 16_000, 16_000);
        let mut out = d.push([1.0, 3.0, 2.0, 4.0, 0.0, 0.0].into_iter());
        out.extend(d.push([6.0, 6.0].into_iter()));
        assert_eq!(out, vec![2.0, 3.0, 0.0]);
    }

    #[test]
    fn downmix_halves_a_48k_stream() {
        let mut d = Downmix::new(1, 48_000, 16_000);
        let total: usize = (0..10).map(|_| d.push((0..480).map(|i| i as f32)).len()).sum();
        assert_eq!(total, 1600);
    }

    #[test]
    fn a_bare_command_word_presses_the_key() {
        let cfg = VoiceConfig::default();
        assert_eq!(command_action("Enter.", &cfg), Some(VoiceAction::Key(b"\r")));
        assert_eq!(command_action("  new line ", &cfg), Some(VoiceAction::Key(b"\r")));
        assert_eq!(command_action("Tab", &cfg), Some(VoiceAction::Key(b"\t")));
        assert_eq!(command_action("Escape!", &cfg), Some(VoiceAction::Key(b"\x1b")));
        assert_eq!(command_action("Next tab.", &cfg), Some(VoiceAction::NextTab));
        assert_eq!(command_action("previous tab", &cfg), Some(VoiceAction::PrevTab));
    }

    #[test]
    fn a_command_word_inside_a_sentence_is_text() {
        let cfg = VoiceConfig::default();
        assert_eq!(command_action("press enter to continue", &cfg), None);
        assert_eq!(command_action("enter enter", &cfg), None);
        assert_eq!(command_action("", &cfg), None);
        assert_eq!(command_action("...", &cfg), None);
    }

    #[test]
    fn navigation_prefixes_carry_the_query() {
        let cfg = VoiceConfig::default();
        assert_eq!(command_action("Switch to build.", &cfg), Some(VoiceAction::Navigate("build".into())));
        assert_eq!(command_action("go to American solarpunk", &cfg), Some(VoiceAction::Navigate("american solarpunk".into())));
        assert_eq!(command_action("Focus status board", &cfg), Some(VoiceAction::Navigate("status board".into())));
        // The bare prefix, or a word that merely starts with it, is text.
        assert_eq!(command_action("switch to", &cfg), None);
        assert_eq!(command_action("focused work", &cfg), None);
        assert_eq!(command_action("switch tomorrow", &cfg), None);
    }

    #[test]
    fn commands_can_be_renamed_or_removed() {
        let mut cfg = VoiceConfig::default();
        cfg.commands.clear();
        cfg.commands.insert("go".to_string(), "enter".to_string());
        assert_eq!(command_action("Go.", &cfg), Some(VoiceAction::Key(b"\r")));
        assert_eq!(command_action("enter", &cfg), None);
        cfg.commands.insert("zap".to_string(), "no-such-key".to_string());
        assert_eq!(command_action("zap", &cfg), None);
        cfg.navigate.clear();
        assert_eq!(command_action("switch to build", &cfg), None);
    }

    #[test]
    fn level_is_zero_for_silence_and_high_for_loud() {
        assert_eq!(level_of(&[0.0; 512]), 0.0);
        assert!(level_of(&[0.5; 512]) > 0.8);
    }

    /// Needs the models: run voice-models.sh, then
    /// `cargo test --release -- --ignored --nocapture voice_transcribes`.
    /// Transcribes every sample wav shipped with the model (they come at
    /// 22.05/24 kHz, so this also exercises the resampler) and prints timings.
    #[test]
    #[ignore]
    fn voice_transcribes_the_bundled_samples() {
        let cfg = VoiceConfig::default();
        let mut wavs = Vec::new();
        walk_for_wavs(&default_model_dir(), &mut wavs);
        assert!(!wavs.is_empty(), "no test wav under {}", default_model_dir().display());
        let t0 = Instant::now();
        let mut engine = Engine::load(&cfg).expect("models");
        eprintln!("models loaded in {} ms", t0.elapsed().as_millis());
        for wav in wavs {
            let (rate, channels, pcm) = read_pcm16_wav(&wav);
            let mut d = Downmix::new(channels, rate, SAMPLE_RATE);
            let samples = d.push(pcm.iter().map(|&s| s as f32 / 32768.0));
            let t0 = Instant::now();
            let text = engine.recognizer.transcribe(SAMPLE_RATE, &samples);
            eprintln!(
                "{}: {:?}  ({:.1}s audio, {} ms)",
                wav.file_name().unwrap().to_string_lossy(),
                text.trim(),
                samples.len() as f32 / SAMPLE_RATE as f32,
                t0.elapsed().as_millis()
            );
            assert!(!text.trim().is_empty(), "{} produced no text", wav.display());
        }
    }

    fn walk_for_wavs(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk_for_wavs(&p, out);
            } else if p.extension().is_some_and(|x| x == "wav") {
                out.push(p);
            }
        }
    }

    /// Minimal RIFF/WAVE reader for 16-bit PCM: (sample rate, channels, samples).
    fn read_pcm16_wav(path: &Path) -> (u32, usize, Vec<i16>) {
        let b = std::fs::read(path).expect("read wav");
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(&b[8..12], b"WAVE");
        let (mut rate, mut channels, mut bits) = (0u32, 0usize, 0u16);
        let mut pcm = Vec::new();
        let mut i = 12;
        while i + 8 <= b.len() {
            let id = &b[i..i + 4];
            let len = u32::from_le_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]) as usize;
            let body = &b[i + 8..(i + 8 + len).min(b.len())];
            match id {
                b"fmt " => {
                    channels = u16::from_le_bytes([body[2], body[3]]) as usize;
                    rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                    bits = u16::from_le_bytes([body[14], body[15]]);
                }
                b"data" => pcm = body.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect(),
                _ => {}
            }
            i += 8 + len + (len & 1);
        }
        assert_eq!(bits, 16, "{} is not 16-bit PCM", path.display());
        (rate, channels, pcm)
    }
}
