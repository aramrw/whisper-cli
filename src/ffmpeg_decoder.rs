use anyhow::{Result, anyhow};
use audrey::Reader;
use std::env::temp_dir;
use std::path::Path;
use std::process::Stdio;
use std::{fs::File, process::Command};

use crate::CLI;

fn use_ffmpeg_with_filter<P: AsRef<Path>>(input_path: P) -> Result<Vec<i16>> {
    let mut command = Command::new("ffmpeg")
        .args([
            "-i", input_path.as_ref().to_str().ok_or_else(|| anyhow!("invalid path"))?,
            // af = audio filter. Remove silence longer than 2 seconds
            // with a noise tolerance of -30dB.
            "-af", "silenceremove=start_periods=1:start_duration=0:stop_periods=-1:stop_duration=2:stop_threshold=-30dB",
            "-ar", "16000",
            "-ac", "1",
            "-c:a", "pcm_s16le",
            "-f", "s16le", // Set format for piping
            "pipe:1", // Output to stdout
            "-hide_banner", "-y", "-loglevel", "error",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped()) // Capture stdout
        .spawn()?;

    let mut stdout = command
        .stdout
        .take()
        .ok_or(anyhow!("Failed to open ffmpeg stdout"))?;

    // Read the raw PCM data from ffmpeg's stdout
    let mut buffer = Vec::new();
    std::io::Read::read_to_end(&mut stdout, &mut buffer)?;

    let status = command.wait()?;
    if !status.success() {
        return Err(anyhow!("ffmpeg filter command failed"));
    }

    // Convert the raw byte buffer to i16 samples
    let samples: Vec<i16> = buffer
        .chunks_exact(2)
        .map(|a| i16::from_le_bytes([a[0], a[1]]))
        .collect();

    Ok(samples)
}

// ffmpeg -i input.mp3 -ar 16000 output.wav
// already forced -ar 16000 -ac 1 -c:a pcm_s16le,
// so sample rate and mono channel expectations are met and the input is i16 PCM.
fn use_ffmpeg<P: AsRef<Path>>(input_path: P) -> Result<Vec<i16>> {
    let temp_file = temp_dir().join(format!("{}.wav", uuid::Uuid::new_v4()));
    let mut pid = Command::new("ffmpeg")
        .args([
            "-i",
            input_path
                .as_ref()
                .to_str()
                .ok_or_else(|| anyhow!("invalid path"))?,
            "-ar",
            "16000",
            "-ac",
            "1",
            "-c:a",
            "pcm_s16le",
            (temp_file.to_str().unwrap()),
            "-hide_banner",
            "-y",
            "-loglevel",
            "error",
        ])
        .stdin(Stdio::null())
        .spawn()?;

    if pid.wait()?.success() {
        let output = File::open(&temp_file)?;
        let mut reader = Reader::new(output)?;
        let samples: Result<Vec<i16>, _> = reader.samples().collect();
        std::fs::remove_file(temp_file)?;
        samples.map_err(std::convert::Into::into)
    } else {
        Err(anyhow!("unable to convert file"))
    }
}

pub fn read_file<P: AsRef<Path>>(audio_file_path: P) -> Result<Vec<f32>> {
    let audio_buf = if CLI.auto_strip {
        println!("running ffmpeg with filter");
        use_ffmpeg_with_filter(&audio_file_path)?
    } else {
        use_ffmpeg(&audio_file_path)?
    };
    // allocate output with same length as input
    let mut output = vec![0.0f32; audio_buf.len()];
    whisper_rs::convert_integer_to_float_audio(&audio_buf, &mut output)?;
    // optional sanity checks
    if output.iter().any(|s| !s.is_finite()) {
        return Err(anyhow!("audio contains NaN/Inf"));
    }
    Ok(output)
}
