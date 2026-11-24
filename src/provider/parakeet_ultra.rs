use crate::{
    live::audio::{
        wisp_parakeet_ultra_available, wisp_parakeet_ultra_free_string,
        wisp_parakeet_ultra_transcribe_file,
    },
    provider::Transcriber,
    transcript::{Transcript, Utternace},
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::{
    ffi::{CStr, CString},
    path::Path,
    time::Instant,
};

pub struct ParakeetUltraProvider {
    model_arg: Option<String>,
    lang: Option<String>,
    silent: bool,
}

impl ParakeetUltraProvider {
    pub fn new(model_arg: Option<String>, lang: Option<String>, silent: bool) -> Result<Self> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (model_arg, lang, silent);
            return Err(anyhow!(
                "Native Parakeet Ultra CoreML provider is only supported on macOS Apple Silicon."
            ));
        }

        #[cfg(target_os = "macos")]
        {
            if !unsafe { wisp_parakeet_ultra_available() } {
                return Err(anyhow!(
                    "Native Parakeet Ultra bridge is not available on this platform."
                ));
            }
            Ok(Self {
                model_arg,
                lang,
                silent,
            })
        }
    }
}

#[derive(Deserialize, Debug)]
struct UltraSpan {
    text: String,
    start_ms: u64,
    end_ms: u64,
}

#[derive(Deserialize, Debug)]
struct ParakeetUltraFileResponse {
    text: Option<String>,
    duration_ms: Option<u64>,
    segments: Option<Vec<UltraSpan>>,
    words: Option<Vec<UltraSpan>>,
    error: Option<String>,
}

#[cfg(target_os = "macos")]
fn run_parakeet_ultra_transcribe_file(
    audio_path: &Path,
    model_arg: Option<&str>,
    lang: Option<&str>,
    silent: bool,
) -> Result<ParakeetUltraFileResponse> {
    let audio_str = audio_path
        .to_str()
        .ok_or_else(|| anyhow!("Audio path is not valid UTF-8"))?;
    let c_audio = CString::new(audio_str)?;

    let model_dir = model_arg
        .map(str::trim)
        .filter(|s| !s.is_empty() && Path::new(s).is_dir());
    let c_model_dir = model_dir.map(CString::new).transpose()?;

    let clean_lang = lang
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto"));
    let c_lang = clean_lang.map(CString::new).transpose()?;

    let raw_ptr = unsafe {
        wisp_parakeet_ultra_transcribe_file(
            c_audio.as_ptr(),
            c_model_dir
                .as_ref()
                .map_or(std::ptr::null(), |c| c.as_ptr()),
            c_lang.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            silent,
        )
    };

    if raw_ptr.is_null() {
        return Err(anyhow!(
            "wisp_parakeet_ultra_transcribe_file returned a null response"
        ));
    }

    let json_str = unsafe {
        let s = CStr::from_ptr(raw_ptr).to_string_lossy().into_owned();
        wisp_parakeet_ultra_free_string(raw_ptr);
        s
    };

    let parsed: ParakeetUltraFileResponse = serde_json::from_str(&json_str).map_err(|e| {
        anyhow!("Failed to parse Parakeet Ultra transcription JSON: {e}\nRaw output: {json_str}")
    })?;

    if let Some(err) = parsed.error {
        return Err(anyhow!("Parakeet Ultra file transcription failed: {err}"));
    }

    Ok(parsed)
}

#[async_trait]
impl Transcriber for ParakeetUltraProvider {
    async fn transcribe(
        &self,
        audio_path: &Path,
        _translate: bool,
        word_timestamps: bool,
    ) -> Result<Transcript> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (audio_path, word_timestamps);
            return Err(anyhow!(
                "Native Parakeet Ultra CoreML provider is only supported on macOS Apple Silicon."
            ));
        }

        #[cfg(target_os = "macos")]
        {
            let start = Instant::now();
            let path_buf = audio_path.to_path_buf();
            let model_arg = self.model_arg.clone();
            let lang = self.lang.clone();
            let silent = self.silent;

            let resp = tokio::task::spawn_blocking(move || {
                run_parakeet_ultra_transcribe_file(
                    &path_buf,
                    model_arg.as_deref(),
                    lang.as_deref(),
                    silent,
                )
            })
            .await??;

            let mut utterances = Vec::new();
            if let Some(segments) = resp.segments {
                for seg in segments {
                    let text = seg.text.trim();
                    if !text.is_empty() {
                        utterances.push(Utternace {
                            start: (seg.start_ms / 10) as i64,
                            stop: (seg.end_ms / 10) as i64,
                            text: text.to_string(),
                        });
                    }
                }
            }
            if utterances.is_empty() {
                if let Some(full) = resp.text {
                    let text = full.trim();
                    if !text.is_empty() {
                        let dur = resp.duration_ms.unwrap_or(1000).max(100);
                        utterances.push(Utternace {
                            start: 0,
                            stop: (dur / 10) as i64,
                            text: text.to_string(),
                        });
                    }
                }
            }

            let word_utterances = if word_timestamps {
                resp.words.map(|words| {
                    words
                        .into_iter()
                        .filter_map(|w| {
                            let text = w.text.trim().to_string();
                            if text.is_empty() {
                                None
                            } else {
                                Some(Utternace {
                                    start: (w.start_ms / 10) as i64,
                                    stop: (w.end_ms / 10) as i64,
                                    text,
                                })
                            }
                        })
                        .collect()
                })
            } else {
                None
            };

            Ok(Transcript {
                processing_time: start.elapsed(),
                utterances,
                word_utterances,
            })
        }
    }
}
