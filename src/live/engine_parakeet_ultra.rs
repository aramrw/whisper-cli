use crate::{
    live::{
        audio::{
            compute_peak, compute_rms, find_best_split_index, whisper_macos_audio_bytes_received,
            whisper_macos_audio_packets_received, whisper_macos_speech_permille,
            wisp_parakeet_ultra_available, wisp_parakeet_ultra_decode,
            wisp_parakeet_ultra_is_busy, wisp_parakeet_ultra_start, wisp_parakeet_ultra_stop,
            MacOsAudioStream, SAMPLE_RATE,
        },
        config::LiveConfig,
        hud::render_live_hud,
        ramp::AdaptiveRampState,
        sub_server::start_sub_server,
        text_filter::is_hallucination,
        vad::SpeechDetector,
    },
    transcript::{Transcript, Utternace},
    utils::format_timestamp,
};
use anyhow::{anyhow, Result};
use std::{
    collections::VecDeque,
    ffi::{c_char, c_void, CStr, CString},
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub enum ParakeetUltraSpeechEvent {
    Partial {
        preview: String,
    },
    Final {
        text: String,
        short_ts: String,
        start_ms: i64,
        duration_ms: u64,
        decode_ms: u64,
    },
}

pub struct ParakeetUltraCbState {
    pub event_tx: tokio::sync::mpsc::UnboundedSender<ParakeetUltraSpeechEvent>,
    pub sub_broadcast_tx: Option<tokio::sync::broadcast::Sender<String>>,
    pub sub_history: Option<Arc<Mutex<VecDeque<serde_json::Value>>>>,
    pub last_id: Arc<Mutex<String>>,
    pub last_finalized_text: Arc<Mutex<String>>,
    pub start_instant: Instant,
    pub all_utterances: Arc<Mutex<Vec<Utternace>>>,
}

pub extern "C" fn parakeet_ultra_speech_callback(
    ctx: *mut c_void,
    text_ptr: *const c_char,
    is_final: bool,
    segment_start_sec: f64,
    duration_sec: f64,
    decode_ms_f64: f64,
) {
    if ctx.is_null() || text_ptr.is_null() {
        return;
    }
    let c_str = unsafe { CStr::from_ptr(text_ptr) };
    let Ok(text) = c_str.to_str() else { return };
    let text = text.trim();
    if text.is_empty() || is_hallucination(text) {
        return;
    }

    let state = unsafe { &*(ctx as *const ParakeetUltraCbState) };
    {
        let mut last_fin = state.last_finalized_text.lock().unwrap();
        if is_final {
            if text.eq_ignore_ascii_case(last_fin.as_str()) {
                return;
            }
            *last_fin = text.to_string();
        }
    }

    let now_ms = state.start_instant.elapsed().as_millis() as i64;
    let start_ms = (segment_start_sec * 1000.0).max(0.0) as i64;
    let duration_ms = (duration_sec * 1000.0).max(120.0) as u64;
    let end_ms = start_ms + duration_ms as i64;
    let lag_ms = (now_ms - end_ms).max(0) as u64;
    let decode_ms = decode_ms_f64.round().max(1.0) as u64;

    let ts = format_timestamp(start_ms / 10, false, ".");
    let short_ts = ts.split('.').next().unwrap_or(&ts).to_string();

    let cue_id = {
        let mut id_lock = state.last_id.lock().unwrap();
        if is_final {
            let id = id_lock.clone();
            *id_lock = uuid::Uuid::new_v4().to_string();
            id
        } else {
            id_lock.clone()
        }
    };

    let cue = serde_json::json!({
        "id": cue_id,
        "text": text,
        "timestamp": short_ts,
        "receivedAt": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
        "clipStartMs": start_ms,
        "clipEndMs": end_ms,
        "durationMs": duration_ms,
        "lagMs": lag_ms,
        "isFinal": is_final,
    });

    if is_final {
        if let Some(ref hist_arc) = state.sub_history {
            let mut hist = hist_arc.lock().unwrap();
            if hist.len() >= 25 {
                hist.pop_front();
            }
            hist.push_back(cue.clone());
        }
    }
    if let Some(ref bcast_tx) = state.sub_broadcast_tx {
        let _ = bcast_tx.send(cue.to_string());
    }

    if is_final {
        let _ = state.event_tx.send(ParakeetUltraSpeechEvent::Final {
            text: text.to_string(),
            short_ts,
            start_ms,
            duration_ms,
            decode_ms,
        });
    } else {
        let _ = state.event_tx.send(ParakeetUltraSpeechEvent::Partial {
            preview: text.to_string(),
        });
    }
}

fn print_parakeet_ultra_banner(config: &LiveConfig, lang_display: &str) {
    if config.silent {
        if config.sub_port > 0 {
            eprintln!(
                "🦜 [whisper] Headless Parakeet Ultra stream on http://127.0.0.1:{}/events (Ctrl+C to stop)",
                config.sub_port
            );
        } else {
            eprintln!("🦜 [whisper] Headless Parakeet Ultra stream active (Ctrl+C to stop)");
        }
        return;
    }
    if !config.stdout_only {
        eprintln!("╭──────────────────────────────────────────────────────────────╮");
        eprintln!("│ 🦜 NVIDIA Parakeet Ultra 0.6B (Native Apple Neural Engine)   │");
        eprintln!("╰──────────────────────────────────────────────────────────────╯");
        eprintln!("  • Engine   : FluidAudio CoreML TDT Ultra (In-Process Warm ANE)");
        eprintln!("  • Hardware : Apple Neural Engine (ANE) • INT8 Encoder (~125x RT)");
        eprintln!("  • Audio    : ScreenCaptureKit (System Audio, No Mic/BlackHole)");
        if config.sub_port > 0 {
            eprintln!(
                "  • Subtitles: http://127.0.0.1:{}/events (Browser Injector)",
                config.sub_port
            );
        }
        eprintln!("  • Language : {lang_display}");
        eprintln!("  • Status   : Loading & warming up Apple Neural Engine...");
    }
}

fn handle_ultra_event(
    event: ParakeetUltraSpeechEvent,
    all_utterances: &Arc<Mutex<Vec<Utternace>>>,
    current_preview: &mut Option<String>,
    silent: bool,
    stdout_only: bool,
) {
    match event {
        ParakeetUltraSpeechEvent::Partial { preview } => {
            if !silent {
                *current_preview = Some(preview);
            }
        }
        ParakeetUltraSpeechEvent::Final {
            text,
            short_ts,
            start_ms,
            duration_ms,
            decode_ms,
        } => {
            *current_preview = None;

            all_utterances.lock().unwrap().push(Utternace {
                start: start_ms / 10,
                stop: (start_ms + duration_ms as i64) / 10,
                text: text.clone(),
            });

            if silent {
                // Headless mode: SSE broadcast only
            } else if stdout_only {
                println!("{text}");
                let _ = std::io::stdout().flush();
            } else {
                eprintln!(
                    "\r\x1b[2K\x1b[32m🦜 [{short_ts}]\x1b[0m  \x1b[1m{text}\x1b[0m \x1b[2m({decode_ms}ms decode)\x1b[0m"
                );
                let _ = std::io::stderr().flush();
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub async fn run_live_parakeet_ultra(_config: LiveConfig) -> Result<Transcript> {
    Err(anyhow!(
        "--parakeet-ultra live audio capture is only supported on macOS (Apple Silicon CoreML)."
    ))
}

#[cfg(target_os = "macos")]
pub async fn run_live_parakeet_ultra(config: LiveConfig) -> Result<Transcript> {
    let session_start = Instant::now();
    let (sub_broadcast_tx, _) = tokio::sync::broadcast::channel::<String>(64);
    let sub_history = Arc::new(Mutex::new(VecDeque::<serde_json::Value>::with_capacity(32)));
    if config.sub_port > 0 {
        start_sub_server(
            config.sub_port,
            sub_broadcast_tx.clone(),
            Arc::clone(&sub_history),
        )
        .await;
    }

    let lang_opt = config
        .lang
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto"));
    let lang_display = lang_opt.unwrap_or("auto (25 European languages)");
    print_parakeet_ultra_banner(&config, lang_display);

    if !unsafe { wisp_parakeet_ultra_available() } {
        return Err(anyhow!(
            "Native Parakeet Ultra bridge is not available on this platform."
        ));
    }

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<ParakeetUltraSpeechEvent>();
    let all_utterances = Arc::new(Mutex::new(Vec::new()));
    let last_id = Arc::new(Mutex::new(uuid::Uuid::new_v4().to_string()));
    let last_finalized_text = Arc::new(Mutex::new(String::new()));

    let cb_state = Arc::new(ParakeetUltraCbState {
        event_tx,
        sub_broadcast_tx: Some(sub_broadcast_tx.clone()),
        sub_history: Some(Arc::clone(&sub_history)),
        last_id: Arc::clone(&last_id),
        last_finalized_text: Arc::clone(&last_finalized_text),
        start_instant: session_start,
        all_utterances: Arc::clone(&all_utterances),
    });

    let custom_model_dir = config
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && Path::new(s).is_dir())
        .map(ToString::to_string);
    let c_model_dir = custom_model_dir
        .as_deref()
        .and_then(|s| CString::new(s).ok());
    let c_lang = lang_opt.and_then(|s| CString::new(s).ok());
    let silent = config.silent;
    let stdout_only = config.stdout_only;

    let state_ptr = Arc::into_raw(cb_state) as *mut c_void;
    let state_addr = state_ptr as usize;

    // Run potentially long first-run CoreML download + ANE warmup off the Tokio executor
    let engine_addr = tokio::task::spawn_blocking(move || {
        let model_dir_ptr = c_model_dir
            .as_ref()
            .map_or(std::ptr::null(), |c| c.as_ptr());
        let lang_ptr = c_lang.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        unsafe {
            wisp_parakeet_ultra_start(
                model_dir_ptr,
                lang_ptr,
                silent,
                state_addr as *mut c_void,
                parakeet_ultra_speech_callback,
            ) as usize
        }
    })
    .await
    .map_err(|e| anyhow!("Parakeet Ultra initialization task panicked: {e}"))?;

    if engine_addr == 0 {
        let _ = unsafe { Arc::from_raw(state_ptr as *const ParakeetUltraCbState) };
        return Err(anyhow!(
            "Failed to initialize Native Parakeet Ultra streaming session"
        ));
    }
    let engine_ptr = engine_addr as *mut c_void;

    if !stdout_only && !silent {
        eprintln!("  • Ready!   : Streaming live... (Press Ctrl+C to stop)\n");
    }

    let (_audio_stream, mut audio_rx) = MacOsAudioStream::start()?;

    let is_realtime = config.preset == Some(crate::server::ServerPreset::Realtime);
    let mut ramp = AdaptiveRampState::new(is_realtime, config.speech_mode, config.chunk_secs);

    let pre_roll_capacity = (0.28 * SAMPLE_RATE as f32) as usize;
    let vad_threshold: f32 = 0.0045;
    let speech_mode = config.speech_mode;

    let mut pre_roll: VecDeque<f32> = VecDeque::with_capacity(pre_roll_capacity + 2048);
    let max_buffer_capacity = (15.0 * SAMPLE_RATE as f32) as usize;
    let mut speech_buffer: Vec<f32> = Vec::with_capacity(max_buffer_capacity + 4096);
    let mut spark_history: VecDeque<f32> = VecDeque::from(vec![0.0; 16]);
    let mut speech_detector = SpeechDetector::new();
    let mut speaking = false;
    let mut silence_samples: usize = 0;
    let mut active_speech_samples: usize = 0;
    let mut segment_start_ms: i64 = 0;
    let mut current_rms: f32 = 0.0;
    let mut current_speech_prob: f32 = 0.0;
    let mut recent_peak: f32 = 0.0;
    let mut tick: usize = 0;
    let mut last_packet_instant = Instant::now();
    let mut last_speech_instant = Instant::now();
    let mut last_partial_instant = Instant::now();
    let mut last_partial_samples: usize = 0;
    let mut current_preview: Option<String> = None;

    // Keep an 80ms tick unconditionally so VAD silence-pause finalization still fires in --silent mode
    let mut vad_interval = tokio::time::interval(Duration::from_millis(80));

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            _ = &mut ctrl_c => {
                if !speech_buffer.is_empty() {
                    let (_, _, _, _, _, short_phrase_min_samples, _, _) = ramp.current_thresholds();
                    if active_speech_samples >= short_phrase_min_samples {
                        unsafe {
                            wisp_parakeet_ultra_decode(
                                engine_ptr,
                                speech_buffer.as_ptr(),
                                speech_buffer.len(),
                                true,
                                segment_start_ms as f64 / 1000.0,
                            );
                        }
                        for _ in 0..50 {
                            if !unsafe { wisp_parakeet_ultra_is_busy(engine_ptr) } {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    }
                }
                break;
            }
            Some(event) = event_rx.recv() => {
                handle_ultra_event(event, &all_utterances, &mut current_preview, silent, stdout_only);
            }
            _ = vad_interval.tick() => {
                if !stdout_only && !silent {
                    let quiet_elapsed = last_packet_instant.elapsed();
                    if quiet_elapsed >= Duration::from_millis(120) {
                        current_rms *= 0.65;
                        recent_peak *= 0.82;
                        current_speech_prob *= 0.75;
                        if current_rms < 0.001 {
                            current_rms = 0.0;
                        }
                        if recent_peak < 0.005 {
                            recent_peak = 0.0;
                        }
                        spark_history.push_back(current_rms);
                        if spark_history.len() > 16 {
                            spark_history.pop_front();
                        }
                        if !speaking && quiet_elapsed >= Duration::from_millis(450) {
                            pre_roll.clear();
                        }
                    }
                } else if !speaking && last_packet_instant.elapsed() >= Duration::from_millis(450) {
                    pre_roll.clear();
                }

                if !speaking {
                    ramp.check_silence_reset();
                } else {
                    let (_, _, _, _, _, short_phrase_min_samples, _, _) = ramp.current_thresholds();

                    let elapsed_silence_samples =
                        (last_speech_instant.elapsed().as_secs_f32() * SAMPLE_RATE as f32) as usize;
                    let effective_silence = silence_samples.max(elapsed_silence_samples);

                    let pause_thresh_secs = if speech_buffer.len() >= (8.5 * SAMPLE_RATE as f32) as usize {
                        0.38
                    } else if speech_buffer.len() >= (5.0 * SAMPLE_RATE as f32) as usize {
                        0.52
                    } else {
                        0.68
                    };
                    let pause_silence_samples = (pause_thresh_secs * SAMPLE_RATE as f32) as usize;
                    let min_phrase_samples = (1.2 * SAMPLE_RATE as f32) as usize;

                    let natural_pause_ready = speech_buffer.len() >= min_phrase_samples
                        && effective_silence >= pause_silence_samples;
                    let short_phrase_ready = active_speech_samples >= short_phrase_min_samples
                        && effective_silence >= (0.62 * SAMPLE_RATE as f32) as usize;
                    let dropped_noise = active_speech_samples < short_phrase_min_samples
                        && effective_silence >= (0.62 * SAMPLE_RATE as f32) as usize;

                    if dropped_noise {
                        speech_buffer.clear();
                        speaking = false;
                        silence_samples = 0;
                        active_speech_samples = 0;
                        current_preview = None;
                    } else if natural_pause_ready || short_phrase_ready {
                        let chunk = std::mem::take(&mut speech_buffer);
                        speaking = false;
                        silence_samples = 0;
                        active_speech_samples = 0;
                        last_partial_samples = 0;

                        if compute_rms(&chunk) >= 0.005 || compute_peak(&chunk) >= 0.015 {
                            let _ = ramp.record_emission();
                            unsafe {
                                wisp_parakeet_ultra_decode(
                                    engine_ptr,
                                    chunk.as_ptr(),
                                    chunk.len(),
                                    true,
                                    segment_start_ms as f64 / 1000.0,
                                );
                            }
                        }
                    }
                }

                if !stdout_only && !silent {
                    tick = tick.wrapping_add(1);
                    let bytes = unsafe { whisper_macos_audio_bytes_received() };
                    let pkts = unsafe { whisper_macos_audio_packets_received() };
                    let is_busy = unsafe { wisp_parakeet_ultra_is_busy(engine_ptr) };
                    let speech_secs = speech_buffer.len() as f32 / SAMPLE_RATE as f32;

                    let (curr_max_samples, _, _, _, _, _, _, _) = ramp.current_thresholds();
                    let curr_max_secs = curr_max_samples as f32 / SAMPLE_RATE as f32;

                    if let Some(ref preview) = current_preview {
                        let truncated = if preview.chars().count() > 54 {
                            let tail: String = preview
                                .chars()
                                .rev()
                                .take(51)
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                                .collect();
                            format!("...{tail}")
                        } else {
                            preview.clone()
                        };
                        eprint!("\r\x1b[2K\x1b[32m🗣️  {truncated}\x1b[0m");
                        let _ = std::io::stderr().flush();
                    } else {
                        let hud = render_live_hud(
                            tick,
                            current_rms,
                            recent_peak,
                            current_speech_prob,
                            &spark_history,
                            speaking,
                            speech_secs,
                            curr_max_secs,
                            is_busy,
                            bytes,
                            pkts,
                            speech_mode,
                            ramp.stage_label(),
                        );
                        eprint!("\r\x1b[2K{hud}");
                        let _ = std::io::stderr().flush();
                    }
                }
            }
            maybe_samples = audio_rx.recv() => {
                let Some(samples) = maybe_samples else {
                    break;
                };

                let rms = compute_rms(&samples);
                let pkt_peak = compute_peak(&samples);
                if pkt_peak < 0.005 && rms < 0.005 {
                    continue;
                }

                last_packet_instant = Instant::now();
                let ane_conf = unsafe { whisper_macos_speech_permille() } as f32 / 1000.0;
                let (detected_voice, speech_prob, _vocal_rms) =
                    speech_detector.analyze(&samples, ane_conf);

                if !stdout_only && !silent {
                    current_rms = rms;
                    current_speech_prob = speech_prob;
                    recent_peak = (recent_peak * 0.95).max(pkt_peak);
                    spark_history.push_back(rms);
                    if spark_history.len() > 16 {
                        spark_history.pop_front();
                    }
                }

                let frame_has_speech = if speech_mode {
                    detected_voice
                } else {
                    rms >= vad_threshold || pkt_peak >= 0.025
                };

                let now_ms = session_start.elapsed().as_millis() as i64;

                if !speaking {
                    ramp.check_silence_reset();
                    if frame_has_speech {
                        speaking = true;
                        last_speech_instant = Instant::now();
                        silence_samples = 0;
                        active_speech_samples = samples.len();
                        let pre_roll_ms = (pre_roll.len() * 1000 / SAMPLE_RATE) as i64;
                        segment_start_ms = (now_ms - pre_roll_ms).max(0);
                        speech_buffer.clear();
                        speech_buffer.extend(pre_roll.iter().copied());
                        speech_buffer.extend_from_slice(&samples);
                        pre_roll.clear();
                        last_partial_samples = 0;
                        last_partial_instant = Instant::now();
                    } else {
                        pre_roll.extend(samples.iter().copied());
                        while pre_roll.len() > pre_roll_capacity {
                            pre_roll.pop_front();
                        }
                    }
                } else {
                    if frame_has_speech {
                        last_speech_instant = Instant::now();
                        silence_samples = 0;
                        active_speech_samples += samples.len();
                    } else {
                        silence_samples += samples.len();
                    }
                    speech_buffer.extend_from_slice(&samples);

                    // Emit snappy live partial hypothesis every ~280ms of new speech
                    let min_partial_samples = (0.45 * SAMPLE_RATE as f32) as usize;
                    let new_samples_since_partial = speech_buffer.len().saturating_sub(last_partial_samples);
                    if speech_buffer.len() >= min_partial_samples
                        && new_samples_since_partial >= (0.25 * SAMPLE_RATE as f32) as usize
                        && last_partial_instant.elapsed() >= Duration::from_millis(280)
                    {
                        let busy = unsafe { wisp_parakeet_ultra_is_busy(engine_ptr) };
                        if !busy {
                            last_partial_instant = Instant::now();
                            last_partial_samples = speech_buffer.len();
                            unsafe {
                                wisp_parakeet_ultra_decode(
                                    engine_ptr,
                                    speech_buffer.as_ptr(),
                                    speech_buffer.len(),
                                    false,
                                    segment_start_ms as f64 / 1000.0,
                                );
                            }
                        }
                    }

                    // Check max buffer limit to prevent runaway sentences
                    let (curr_max_samples, _, _, _, _, _, _, _) = ramp.current_thresholds();
                    if speech_buffer.len() >= curr_max_samples {
                        let overlap_samples = (0.12 * SAMPLE_RATE as f32) as usize;
                        let split_idx = find_best_split_index(&speech_buffer, overlap_samples);
                        let tail = speech_buffer[split_idx.saturating_sub(overlap_samples)..].to_vec();
                        let to_send = speech_buffer[..split_idx].to_vec();

                        let sent_dur_ms = (split_idx.saturating_sub(overlap_samples) as i64 * 1000) / SAMPLE_RATE as i64;
                        let _ = ramp.record_emission();
                        unsafe {
                            wisp_parakeet_ultra_decode(
                                engine_ptr,
                                to_send.as_ptr(),
                                to_send.len(),
                                true,
                                segment_start_ms as f64 / 1000.0,
                            );
                        }
                        segment_start_ms += sent_dur_ms;
                        speech_buffer = tail;
                        active_speech_samples = speech_buffer.len();
                        silence_samples = 0;
                        last_partial_samples = 0;
                    }
                }
            }
        }
    }

    if !stdout_only && !silent {
        eprintln!("\n👋 Stopping Parakeet Ultra native streaming capture...");
    }

    unsafe {
        wisp_parakeet_ultra_stop(engine_ptr);
    }

    while let Ok(event) = event_rx.try_recv() {
        handle_ultra_event(event, &all_utterances, &mut current_preview, silent, stdout_only);
    }

    let final_cb_state = unsafe { Arc::from_raw(state_ptr as *const ParakeetUltraCbState) };
    let utts = final_cb_state.all_utterances.lock().unwrap().clone();

    Ok(Transcript {
        processing_time: session_start.elapsed(),
        utterances: utts,
        word_utterances: None,
    })
}
