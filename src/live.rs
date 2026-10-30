use crate::{
    provider::Transcriber,
    transcript::{Transcript, Utternace},
    utils::format_timestamp,
};
use anyhow::{anyhow, Result};
use std::{
    collections::VecDeque,
    env::temp_dir,
    ffi::{c_char, c_void, CStr},
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

const SAMPLE_RATE: usize = 16_000;
const SPARK_CHARS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[cfg(target_os = "macos")]
type WhisperAudioCallback = extern "C" fn(samples: *const f32, count: usize, user_data: *mut c_void);

#[cfg(target_os = "macos")]
extern "C" {
    fn whisper_macos_audio_start(
        callback: WhisperAudioCallback,
        user_data: *mut c_void,
        error_out: *mut *mut c_char,
    ) -> *mut c_void;
    fn whisper_macos_audio_stop(handle: *mut c_void);
    fn whisper_macos_free_error(err: *mut c_char);
    fn whisper_macos_audio_packets_received() -> u64;
    fn whisper_macos_video_packets_received() -> u64;
    fn whisper_macos_audio_bytes_received() -> u64;
    fn whisper_macos_audio_last_status() -> i32;
    fn whisper_macos_audio_sample_rate() -> u32;
}

#[cfg(target_os = "macos")]
extern "C" fn audio_callback_trampoline(
    samples: *const f32,
    count: usize,
    user_data: *mut c_void,
) {
    if samples.is_null() || count == 0 || user_data.is_null() {
        return;
    }
    let slice = unsafe { std::slice::from_raw_parts(samples, count) };
    let sender = unsafe { &*(user_data as *const mpsc::UnboundedSender<Vec<f32>>) };
    let _ = sender.send(slice.to_vec());
}

#[cfg(target_os = "macos")]
struct MacOsAudioStream {
    handle: *mut c_void,
    sender_ptr: *mut mpsc::UnboundedSender<Vec<f32>>,
}

#[cfg(target_os = "macos")]
unsafe impl Send for MacOsAudioStream {}

#[cfg(target_os = "macos")]
impl MacOsAudioStream {
    fn start() -> Result<(Self, mpsc::UnboundedReceiver<Vec<f32>>)> {
        let (tx, rx) = mpsc::unbounded_channel();
        let sender_box = Box::new(tx);
        let sender_ptr = Box::into_raw(sender_box);
        let mut error_out: *mut c_char = std::ptr::null_mut();

        let handle = unsafe {
            whisper_macos_audio_start(
                audio_callback_trampoline,
                sender_ptr.cast::<c_void>(),
                &mut error_out,
            )
        };

        if handle.is_null() {
            unsafe {
                let _ = Box::from_raw(sender_ptr);
            }
            let err_msg = if error_out.is_null() {
                "Unknown error starting macOS ScreenCaptureKit audio stream".to_string()
            } else {
                let msg = unsafe { CStr::from_ptr(error_out) }
                    .to_string_lossy()
                    .into_owned();
                unsafe {
                    whisper_macos_free_error(error_out);
                }
                msg
            };
            return Err(anyhow!(err_msg));
        }

        Ok((
            Self {
                handle,
                sender_ptr,
            },
            rx,
        ))
    }
}

#[cfg(target_os = "macos")]
impl Drop for MacOsAudioStream {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                whisper_macos_audio_stop(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
        if !self.sender_ptr.is_null() {
            unsafe {
                let _ = Box::from_raw(self.sender_ptr);
            }
            self.sender_ptr = std::ptr::null_mut();
        }
    }
}

struct TempWavFile {
    path: PathBuf,
}

impl TempWavFile {
    fn new(samples: &[f32]) -> Result<Self> {
        let path = temp_dir().join(format!("whisper_live_{}.wav", uuid::Uuid::new_v4()));
        write_pcm16_wav(&path, samples, SAMPLE_RATE as u32)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempWavFile {
    fn drop(&mut self) {
        if self.path.exists() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn write_pcm16_wav(path: &Path, samples: &[f32], sample_rate: u32) -> Result<()> {
    let max_abs = samples
        .iter()
        .fold(0.0_f32, |acc, &s| acc.max(s.abs()));

    // Gently normalize low-volume streams so Whisper still receives a clear signal
    let gain = if max_abs > 0.006 && max_abs < 0.35 {
        (0.70 / max_abs).min(8.0)
    } else {
        1.0
    };

    let data_bytes = (samples.len() * 2) as u32;
    let riff_size = 36 + data_bytes;
    let byte_rate = sample_rate * 2;

    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    writer.write_all(b"RIFF")?;
    writer.write_all(&riff_size.to_le_bytes())?;
    writer.write_all(b"WAVE")?;
    writer.write_all(b"fmt ")?;
    writer.write_all(&16_u32.to_le_bytes())?; // PCM fmt chunk size
    writer.write_all(&1_u16.to_le_bytes())?; // AudioFormat = 1 (PCM)
    writer.write_all(&1_u16.to_le_bytes())?; // NumChannels = 1 (Mono)
    writer.write_all(&sample_rate.to_le_bytes())?;
    writer.write_all(&byte_rate.to_le_bytes())?;
    writer.write_all(&2_u16.to_le_bytes())?; // BlockAlign = 2
    writer.write_all(&16_u16.to_le_bytes())?; // BitsPerSample = 16
    writer.write_all(b"data")?;
    writer.write_all(&data_bytes.to_le_bytes())?;

    for &sample in samples {
        let scaled = (sample * gain).clamp(-1.0, 1.0);
        let pcm16 = (scaled * 32767.0) as i16;
        writer.write_all(&pcm16.to_le_bytes())?;
    }

    writer.flush()?;
    Ok(())
}

fn compute_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|&s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

fn compute_peak(samples: &[f32]) -> f32 {
    samples
        .iter()
        .fold(0.0_f32, |acc, &s| acc.max(s.abs()))
}

/// Find the quietest 25ms window within the last `search_window` samples to split continuous speech cleanly.
fn find_best_split_index(samples: &[f32], search_window: usize) -> usize {
    let len = samples.len();
    let window = 400; // 25ms at 16kHz
    if len <= search_window || search_window <= window * 2 {
        return len;
    }

    let start_search = len - search_window;
    let mut best_idx = len;
    let mut min_energy = f32::MAX;

    let step = 80; // 5ms step
    let mut idx = start_search;
    while idx + window <= len {
        let energy: f32 = samples[idx..idx + window].iter().map(|&s| s * s).sum();
        if energy < min_energy {
            min_energy = energy;
            best_idx = idx + window / 2;
        }
        idx += step;
    }

    best_idx
}

fn is_hallucination(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return true;
    }

    // Filter out pure bracketed/parenthesized non-speech tags like [Music], (Laughter), ♪
    if (trimmed.starts_with('[') && trimmed.ends_with(']'))
        || (trimmed.starts_with('(') && trimmed.ends_with(')'))
        || (trimmed.starts_with('♪') && trimmed.ends_with('♪'))
    {
        return true;
    }

    let cleaned: String = trimmed
        .chars()
        .filter(|c| {
            !c.is_ascii_punctuation()
                && !matches!(*c, '¡' | '¿' | '♪' | '…' | '。' | '、' | '！' | '？')
        })
        .collect::<String>()
        .to_lowercase();
    let cleaned = cleaned.trim();

    if cleaned.is_empty() {
        return true;
    }

    const HALLUCINATIONS: &[&str] = &[
        "thank you for watching",
        "thanks for watching",
        "thank you very much for watching",
        "subtitles by the amaraorg community",
        "subtitles by",
        "transcribed by",
        "please subscribe",
        "like and subscribe",
        "suscríbete al canal",
        "suscribete al canal",
        "subtítulos realizados por la comunidad de amaraorg",
        "subtitulos realizados por la comunidad de amaraorg",
        "gracias por ver el video",
        "ご視聴ありがとうございました",
        "チャンネル登録よろしくお願いします",
        "チャンネル登録お願いします",
        "字幕",
        "音楽",
        "拍手",
        "music",
        "applause",
        "laughter",
        "you",
    ];

    HALLUCINATIONS.contains(&cleaned)
}

fn clean_subtitle_text(transcript: &Transcript) -> String {
    let mut parts = Vec::new();
    for u in &transcript.utterances {
        let t = u.text.trim();
        if !t.is_empty() && !is_hallucination(t) {
            parts.push(t);
        }
    }
    let joined = parts.join(" ");
    if is_hallucination(&joined) {
        String::new()
    } else {
        joined
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn render_sparkline(history: &VecDeque<f32>) -> String {
    history
        .iter()
        .map(|&rms| {
            let norm = (rms / 0.10).clamp(0.0, 1.0).sqrt();
            let idx = ((norm * (SPARK_CHARS.len() - 1) as f32).round() as usize)
                .min(SPARK_CHARS.len() - 1);
            SPARK_CHARS[idx]
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn render_live_hud(
    tick: usize,
    rms: f32,
    peak: f32,
    spark_history: &VecDeque<f32>,
    is_speaking: bool,
    speech_secs: f32,
    max_secs: f32,
    in_flight: bool,
    audio_bytes: u64,
    audio_pkts: u64,
) -> String {
    let spinner = SPINNER_FRAMES[tick % SPINNER_FRAMES.len()];
    let normalized = (rms / 0.12).clamp(0.0, 1.0).sqrt();
    let filled = (normalized * 10.0).round() as usize;
    let empty = 10_usize.saturating_sub(filled);

    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(empty));
    let spark = render_sparkline(spark_history);
    let bytes_str = format_bytes(audio_bytes);

    let state = if in_flight {
        "\x1b[33m⏳ Sending\x1b[0m".to_string()
    } else if is_speaking {
        format!("\x1b[32m🗣️ {speech_secs:.1}s/{max_secs:.0}s\x1b[0m")
    } else if audio_bytes == 0 {
        "\x1b[2m⚪ Waiting for macOS audio...\x1b[0m".to_string()
    } else {
        "\x1b[36m🎙️ Listening\x1b[0m".to_string()
    };

    format!(
        "{spinner} [{bar}] \x1b[36m{spark}\x1b[0m │ 📥 {bytes_str} ({audio_pkts} pkts) │ Peak: {peak:.3} │ {state}"
    )
}

pub struct LiveConfig {
    pub provider_label: String,
    pub lang: Option<String>,
    pub translate: bool,
    pub karaoke: bool,
    pub stdout_only: bool,
    pub chunk_secs: f32,
}

struct AudioSegment {
    samples: Vec<f32>,
    start_offset_ms: i64,
    end_offset_ms: i64,
}

#[cfg(not(target_os = "macos"))]
pub async fn run_audio_test() -> Result<()> {
    Err(anyhow!(
        "--test-audio is only supported on macOS (via native ScreenCaptureKit)."
    ))
}

#[cfg(target_os = "macos")]
pub async fn run_audio_test() -> Result<()> {
    let (_stream, mut audio_rx) = MacOsAudioStream::start()?;
    let start = Instant::now();

    eprintln!("╭──────────────────────────────────────────────────────────────╮");
    eprintln!("│ 🎛️  macOS Native System Audio Diagnostic Monitor             │");
    eprintln!("╰──────────────────────────────────────────────────────────────╯");
    eprintln!("  Play any video/music (Crunchyroll, YouTube, Spotify) to see live bytes & waveform.");
    eprintln!("  Press Ctrl+C to stop.\n");

    let mut spark_history: VecDeque<f32> = VecDeque::from(vec![0.0; 16]);
    let mut current_rms: f32 = 0.0;
    let mut max_session_peak: f32 = 0.0;
    let mut tick: usize = 0;
    let mut interval = tokio::time::interval(Duration::from_millis(80));

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprint!("\r\x1b[2K");
                let bytes = unsafe { whisper_macos_audio_bytes_received() };
                let pkts = unsafe { whisper_macos_audio_packets_received() };
                eprintln!(
                    "\n✅ Audio test finished in {:.1}s — Received {} across {} packets (Max Peak: {:.3}).",
                    start.elapsed().as_secs_f32(),
                    format_bytes(bytes),
                    pkts,
                    max_session_peak
                );
                break;
            }
            maybe_samples = audio_rx.recv() => {
                if let Some(samples) = maybe_samples {
                    current_rms = compute_rms(&samples);
                    let pkt_peak = compute_peak(&samples);
                    if pkt_peak > max_session_peak {
                        max_session_peak = pkt_peak;
                    }
                    spark_history.push_back(current_rms);
                    if spark_history.len() > 16 {
                        spark_history.pop_front();
                    }
                }
            }
            _ = interval.tick() => {
                tick = tick.wrapping_add(1);
                let bytes = unsafe { whisper_macos_audio_bytes_received() };
                let pkts = unsafe { whisper_macos_audio_packets_received() };
                let sr = unsafe { whisper_macos_audio_sample_rate() };
                let is_speaking = current_rms >= 0.0045;

                let hud = render_live_hud(
                    tick,
                    current_rms,
                    max_session_peak,
                    &spark_history,
                    is_speaking,
                    0.0,
                    4.0,
                    false,
                    bytes,
                    pkts,
                );
                eprint!("\r\x1b[2K{hud} ({sr}Hz)");
                let _ = std::io::stderr().flush();

                // Gently decay visual meter when idle between packets
                current_rms *= 0.80;
                spark_history.push_back(current_rms);
                if spark_history.len() > 16 {
                    spark_history.pop_front();
                }
            }
        }
    }

    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub async fn run_live_subtitles(
    _transcriber: Arc<Box<dyn Transcriber>>,
    _config: LiveConfig,
) -> Result<Transcript> {
    Err(anyhow!(
        "--live system audio capture is only supported on macOS (via native ScreenCaptureKit)."
    ))
}

#[cfg(target_os = "macos")]
pub async fn run_live_subtitles(
    transcriber: Arc<Box<dyn Transcriber>>,
    config: LiveConfig,
) -> Result<Transcript> {
    let (_stream, mut audio_rx) = MacOsAudioStream::start()?;
    let session_start = Instant::now();

    if !config.stdout_only {
        let lang_str = config.lang.as_deref().unwrap_or("auto-detect");
        let mode_str = if config.translate {
            format!("{lang_str} -> English (translate)")
        } else {
            lang_str.to_string()
        };
        eprintln!("╭──────────────────────────────────────────────────────────────╮");
        eprintln!("│ 🎧 macOS Live System Audio Subtitles (ScreenCaptureKit)      │");
        eprintln!("╰──────────────────────────────────────────────────────────────╯");
        eprintln!("  • Provider : {}", config.provider_label);
        eprintln!("  • Language : {mode_str}");
        eprintln!(
            "  • Window   : {:.1}s adaptive VAD chunks (Press Ctrl+C to stop)\n",
            config.chunk_secs
        );
    }

    let (segment_tx, mut segment_rx) = mpsc::channel::<AudioSegment>(8);
    let is_transcribing = Arc::new(AtomicBool::new(false));
    let is_transcribing_ui = Arc::clone(&is_transcribing);

    let max_chunk_samples = ((config.chunk_secs.clamp(1.5, 15.0)) * SAMPLE_RATE as f32) as usize;
    let min_pause_split_samples = (1.10 * SAMPLE_RATE as f32) as usize;
    let short_phrase_min_samples = (0.32 * SAMPLE_RATE as f32) as usize;
    let pause_silence_samples = (0.36 * SAMPLE_RATE as f32) as usize;
    let long_silence_samples = (0.68 * SAMPLE_RATE as f32) as usize;
    let pre_roll_capacity = (0.30 * SAMPLE_RATE as f32) as usize;
    let overlap_samples = (0.18 * SAMPLE_RATE as f32) as usize;
    let vad_threshold: f32 = 0.0045;

    let stdout_only = config.stdout_only;

    // Background worker that transcribes queued speech segments sequentially in order
    let translate = config.translate;
    let karaoke = config.karaoke;
    let transcriber_worker = Arc::clone(&transcriber);

    let worker_handle = tokio::spawn(async move {
        let mut all_utterances: Vec<Utternace> = Vec::new();
        let mut last_printed = String::new();

        while let Some(seg) = segment_rx.recv().await {
            is_transcribing.store(true, Ordering::Relaxed);

            if let Ok(wav) = TempWavFile::new(&seg.samples) {
                match transcriber_worker
                    .transcribe(wav.path(), translate, karaoke)
                    .await
                {
                    Ok(transcript) => {
                        let subtitle = clean_subtitle_text(&transcript);
                        if !subtitle.is_empty() && subtitle != last_printed {
                            if stdout_only {
                                println!("{subtitle}");
                                let _ = std::io::stdout().flush();
                            } else {
                                let ts = format_timestamp(seg.start_offset_ms / 10, false, ".");
                                let short_ts = ts.split('.').next().unwrap_or(&ts);
                                eprint!("\r\x1b[2K");
                                println!("\x1b[2m[{short_ts}]\x1b[0m  \x1b[1m{subtitle}\x1b[0m");
                                let _ = std::io::stdout().flush();
                            }
                            last_printed = subtitle.clone();
                            all_utterances.push(Utternace {
                                start: seg.start_offset_ms / 10,
                                stop: seg.end_offset_ms / 10,
                                text: subtitle,
                            });
                        }
                    }
                    Err(err) => {
                        if !stdout_only {
                            eprint!("\r\x1b[2K");
                            eprintln!("\x1b[33m⚠️  Transcription error: {err}\x1b[0m");
                        }
                    }
                }
            }

            is_transcribing.store(false, Ordering::Relaxed);
        }

        all_utterances
    });

    let mut pre_roll: VecDeque<f32> = VecDeque::with_capacity(pre_roll_capacity + 2048);
    let mut speech_buffer: Vec<f32> = Vec::with_capacity(max_chunk_samples + 4096);
    let mut spark_history: VecDeque<f32> = VecDeque::from(vec![0.0; 16]);
    let mut speaking = false;
    let mut silence_samples: usize = 0;
    let mut active_speech_samples: usize = 0;
    let mut segment_start_ms: i64 = 0;
    let mut current_rms: f32 = 0.0;
    let mut recent_peak: f32 = 0.0;
    let mut tick: usize = 0;
    let mut ui_interval = tokio::time::interval(Duration::from_millis(80));

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                if !stdout_only {
                    eprint!("\r\x1b[2K");
                    eprintln!("\n👋 Stopping live audio capture...");
                }
                break;
            }
            _ = ui_interval.tick() => {
                if !stdout_only {
                    tick = tick.wrapping_add(1);
                    let bytes = unsafe { whisper_macos_audio_bytes_received() };
                    let pkts = unsafe { whisper_macos_audio_packets_received() };
                    let in_flight = is_transcribing_ui.load(Ordering::Relaxed);
                    let speech_secs = speech_buffer.len() as f32 / SAMPLE_RATE as f32;

                    let hud = render_live_hud(
                        tick,
                        current_rms,
                        recent_peak,
                        &spark_history,
                        speaking,
                        speech_secs,
                        config.chunk_secs,
                        in_flight,
                        bytes,
                        pkts,
                    );
                    eprint!("\r\x1b[2K{hud}");
                    let _ = std::io::stderr().flush();

                    // Decay visual meter slightly when no packets arrived in the last tick
                    current_rms *= 0.82;
                }
            }
            maybe_samples = audio_rx.recv() => {
                let Some(samples) = maybe_samples else {
                    break;
                };

                let rms = compute_rms(&samples);
                let pkt_peak = compute_peak(&samples);
                current_rms = rms;
                recent_peak = (recent_peak * 0.95).max(pkt_peak);
                spark_history.push_back(rms);
                if spark_history.len() > 16 {
                    spark_history.pop_front();
                }

                let frame_has_speech = rms >= vad_threshold || pkt_peak >= 0.025;
                let now_ms = session_start.elapsed().as_millis() as i64;

                if !speaking {
                    if frame_has_speech {
                        speaking = true;
                        silence_samples = 0;
                        active_speech_samples = samples.len();
                        let pre_roll_ms = (pre_roll.len() * 1000 / SAMPLE_RATE) as i64;
                        segment_start_ms = (now_ms - pre_roll_ms).max(0);
                        speech_buffer.clear();
                        speech_buffer.extend(pre_roll.iter().copied());
                        speech_buffer.extend_from_slice(&samples);
                        pre_roll.clear();
                    } else {
                        pre_roll.extend(samples.iter().copied());
                        while pre_roll.len() > pre_roll_capacity {
                            pre_roll.pop_front();
                        }
                    }
                    continue;
                }

                // Currently in an active speech segment
                speech_buffer.extend_from_slice(&samples);
                if frame_has_speech {
                    silence_samples = 0;
                    active_speech_samples += samples.len();
                } else {
                    silence_samples += samples.len();
                }

                let natural_pause_ready = speech_buffer.len() >= min_pause_split_samples
                    && silence_samples >= pause_silence_samples;
                let short_phrase_ready = active_speech_samples >= short_phrase_min_samples
                    && silence_samples >= long_silence_samples;
                let dropped_noise = active_speech_samples < short_phrase_min_samples
                    && silence_samples >= long_silence_samples;

                if dropped_noise {
                    // Brief click/noise burst that wasn't real speech; reset
                    speech_buffer.clear();
                    speaking = false;
                    silence_samples = 0;
                    active_speech_samples = 0;
                } else if natural_pause_ready || short_phrase_ready {
                    let chunk = std::mem::take(&mut speech_buffer);
                    speaking = false;
                    silence_samples = 0;
                    active_speech_samples = 0;

                    let _ = segment_tx.try_send(AudioSegment {
                        samples: chunk,
                        start_offset_ms: segment_start_ms,
                        end_offset_ms: now_ms,
                    });
                } else if speech_buffer.len() >= max_chunk_samples {
                    // Continuous fast speech reached max window: split at quietest micro-pause in last 0.45s
                    let search_win = (0.45 * SAMPLE_RATE as f32) as usize;
                    let split_at = find_best_split_index(&speech_buffer, search_win);

                    let chunk = speech_buffer[..split_at].to_vec();
                    let keep_from = split_at.saturating_sub(overlap_samples);
                    let remainder = speech_buffer[keep_from..].to_vec();

                    let _ = segment_tx.try_send(AudioSegment {
                        samples: chunk,
                        start_offset_ms: segment_start_ms,
                        end_offset_ms: now_ms,
                    });

                    speech_buffer = remainder;
                    let rem_ms = (speech_buffer.len() * 1000 / SAMPLE_RATE) as i64;
                    segment_start_ms = (now_ms - rem_ms).max(0);
                    silence_samples = 0;
                    active_speech_samples = speech_buffer.len();
                }
            }
        }
    }

    // Flush any remaining speech on exit
    if speaking && active_speech_samples >= short_phrase_min_samples {
        let now_ms = session_start.elapsed().as_millis() as i64;
        let _ = segment_tx
            .send(AudioSegment {
                samples: speech_buffer,
                start_offset_ms: segment_start_ms,
                end_offset_ms: now_ms,
            })
            .await;
    }
    drop(segment_tx);

    let utterances = worker_handle.await.unwrap_or_default();

    Ok(Transcript {
        processing_time: session_start.elapsed(),
        utterances,
        word_utterances: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hallucination_filter() {
        assert!(is_hallucination("[Music]"));
        assert!(is_hallucination("(Laughter)"));
        assert!(is_hallucination("Thank you for watching!"));
        assert!(is_hallucination("ご視聴ありがとうございました。"));
        assert!(is_hallucination(
            "Subtítulos realizados por la comunidad de Amara.org"
        ));
        assert!(!is_hallucination("Hola, ¿cómo estás?"));
        assert!(!is_hallucination("これはテストです。"));
    }

    #[test]
    fn test_temp_wav_encoding() {
        let samples: Vec<f32> = (0..1600)
            .map(|i| (i as f32 * 0.1).sin() * 0.2)
            .collect();
        let wav = TempWavFile::new(&samples).expect("should create temp wav");
        let bytes = std::fs::read(wav.path()).expect("should read temp wav");
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(bytes.len(), 44 + samples.len() * 2);
    }

    #[test]
    fn test_find_best_split_index() {
        let mut samples = vec![0.2_f32; 8000];
        for s in &mut samples[6400..6900] {
            *s = 0.0001;
        }
        let split = find_best_split_index(&samples, 2000);
        assert!(split >= 6400 && split <= 6900, "split was {split}");
    }
}
