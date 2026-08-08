use anyhow::{anyhow, Result};
use std::env::temp_dir;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct PreparedAudio {
    pub path: PathBuf,
    is_temp: bool,
}

impl PreparedAudio {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PreparedAudio {
    fn drop(&mut self) {
        if self.is_temp && self.path.exists() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub fn prepare_audio<P: AsRef<Path>>(input_path: P) -> Result<PreparedAudio> {
    let input = input_path.as_ref();
    let ext = input
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // If it's already a wav file, use directly
    if ext == "wav" {
        return Ok(PreparedAudio {
            path: input.to_path_buf(),
            is_temp: false,
        });
    }

    // Convert mp4, m4a, mp3, flac, ogg, etc. to 16kHz mono wav via ffmpeg
    let temp_file = temp_dir().join(format!("{}.wav", uuid::Uuid::new_v4()));
    let status = Command::new("ffmpeg")
        .args([
            "-i",
            input.to_str().ok_or_else(|| anyhow!("invalid path"))?,
            "-ar",
            "16000",
            "-ac",
            "1",
            "-c:a",
            "pcm_s16le",
            temp_file.to_str().unwrap(),
            "-hide_banner",
            "-y",
            "-loglevel",
            "error",
        ])
        .stdin(Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => Ok(PreparedAudio {
            path: temp_file,
            is_temp: true,
        }),
        _ => {
            // Fall back to original file if ffmpeg is unavailable or fails
            Ok(PreparedAudio {
                path: input.to_path_buf(),
                is_temp: false,
            })
        }
    }
}
