#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use anyhow::{anyhow, Result};
use clap::{Parser, ValueEnum};
use std::{env, path::Path, sync::Arc};
use utils::write_to;

mod utils;

use whisper_cli::{
    live::{run_audio_test, run_live_subtitles, LiveConfig},
    CustomApiProvider, DeepgramProvider, GroqProvider, OpenAiProvider, Transcriber,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum ProviderKind {
    #[clap(name = "groq")]
    Groq,
    #[clap(name = "openai")]
    OpenAi,
    #[clap(name = "deepgram")]
    Deepgram,
    #[clap(name = "custom")]
    Custom,
}

#[derive(Parser)]
#[command(
    author,
    version,
    about = "Transcribe audio files or live macOS system audio using modern Speech-to-Text APIs.",
    long_about = "Generate a transcript of an audio file or stream real-time subtitles from macOS system audio using Groq, OpenAI, Deepgram, or a custom OpenAI-compatible API endpoint (like whisper-server)."
)]
struct Args {
    /// Speech-to-Text provider to use (groq, openai, deepgram, custom)
    #[clap(short, long, value_enum, default_value = "groq")]
    provider: ProviderKind,

    /// Name of the model to use (e.g. whisper-large-v3-turbo, whisper-1, nova-3)
    #[clap(short, long)]
    model: Option<String>,

    /// Language spoken in the audio (ISO 639-1 code e.g. 'en', 'es', 'ja'). Auto-detected by default.
    #[clap(short = 'l', long)]
    lang: Option<String>,

    /// Path to the audio file to transcribe (optional when using --live or --test-audio)
    #[clap(required_unless_present_any = ["live", "test_audio"])]
    audio: Option<String>,

    /// Capture macOS system audio in real time and stream subtitles to the terminal
    #[clap(long, default_value = "false")]
    live: bool,

    /// Test macOS system audio capture with a live waveform & byte monitor (no server or API key needed)
    #[clap(long, default_value = "false")]
    test_audio: bool,

    /// Maximum audio chunk duration in seconds for --live mode
    #[clap(long, default_value = "4.0")]
    chunk_secs: f32,

    /// API Key for the chosen provider (defaults to GROQ_API_KEY, OPENAI_API_KEY, or DEEPGRAM_API_KEY env var)
    #[clap(short = 'k', long)]
    api_key: Option<String>,

    /// Custom API Base URL for local/self-hosted OpenAI-compatible speech-to-text endpoints (defaults to http://localhost:8080 for custom provider)
    #[clap(short = 'u', long)]
    api_url: Option<String>,

    /// Toggle translation to English
    #[clap(short, long, default_value = "false")]
    translate: bool,

    /// Generate timestamps for each word
    #[clap(long, default_value = "false")]
    karaoke: bool,

    /// Print transcript directly to terminal stdout
    #[clap(short = 's', long, default_value = "false")]
    stdout: bool,

    /// Skip writing .txt, .vtt, and .srt files to disk
    #[clap(long, default_value = "false")]
    no_files: bool,
}

fn provider_label(args: &Args) -> String {
    match args.provider {
        ProviderKind::Groq => format!(
            "Groq ({})",
            args.model.as_deref().unwrap_or("whisper-large-v3-turbo")
        ),
        ProviderKind::OpenAi => {
            format!("OpenAI ({})", args.model.as_deref().unwrap_or("whisper-1"))
        }
        ProviderKind::Deepgram => {
            format!("Deepgram ({})", args.model.as_deref().unwrap_or("nova-3"))
        }
        ProviderKind::Custom => {
            let url = args.api_url.as_deref().unwrap_or("http://localhost:8080");
            format!("Custom ({url})")
        }
    }
}

fn create_transcriber(args: &Args) -> Result<Box<dyn Transcriber>> {
    let key_from_env = match args.provider {
        ProviderKind::Groq => env::var("GROQ_API_KEY").ok(),
        ProviderKind::OpenAi => env::var("OPENAI_API_KEY").ok(),
        ProviderKind::Deepgram => env::var("DEEPGRAM_API_KEY").ok(),
        ProviderKind::Custom => env::var("CUSTOM_API_KEY").ok(),
    };

    let api_key = args.api_key.clone().or(key_from_env);

    match args.provider {
        ProviderKind::Groq => {
            let key = api_key.ok_or_else(|| {
                anyhow!("Groq API key required. Provide --api-key or set GROQ_API_KEY env var.")
            })?;
            Ok(Box::new(GroqProvider::new(
                key,
                args.model.clone(),
                args.lang.clone(),
            )))
        }
        ProviderKind::OpenAi => {
            let key = api_key.ok_or_else(|| {
                anyhow!("OpenAI API key required. Provide --api-key or set OPENAI_API_KEY env var.")
            })?;
            Ok(Box::new(OpenAiProvider::new(
                key,
                args.model.clone(),
                args.lang.clone(),
            )))
        }
        ProviderKind::Deepgram => {
            let key = api_key.ok_or_else(|| {
                anyhow!("Deepgram API key required. Provide --api-key or set DEEPGRAM_API_KEY env var.")
            })?;
            Ok(Box::new(DeepgramProvider::new(
                key,
                args.model.clone(),
                args.lang.clone(),
            )))
        }
        ProviderKind::Custom => {
            let url = args
                .api_url
                .clone()
                .unwrap_or_else(|| "http://localhost:8080".to_string());
            Ok(Box::new(CustomApiProvider::new(
                url,
                api_key,
                args.model.clone(),
                args.lang.clone(),
            )))
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    if args.test_audio {
        return run_audio_test().await;
    }

    let transcriber = create_transcriber(&args)?;

    if args.live {
        let config = LiveConfig {
            provider_label: provider_label(&args),
            lang: args.lang.clone(),
            translate: args.translate,
            karaoke: args.karaoke,
            stdout_only: args.stdout,
            chunk_secs: args.chunk_secs,
        };
        let transcript = run_live_subtitles(Arc::new(transcriber), config).await?;

        if let Some(output_arg) = &args.audio {
            if !args.no_files && !args.stdout && !transcript.utterances.is_empty() {
                let out_path = Path::new(output_arg);
                let file_name = out_path
                    .file_name()
                    .ok_or_else(|| anyhow!("Invalid output path"))?
                    .to_str()
                    .ok_or_else(|| anyhow!("Non-UTF8 filename"))?;

                write_to(
                    out_path.with_file_name(format!("{file_name}.txt")),
                    &transcript.as_text(),
                );
                write_to(
                    out_path.with_file_name(format!("{file_name}.vtt")),
                    &transcript.as_vtt(),
                );
                write_to(
                    out_path.with_file_name(format!("{file_name}.srt")),
                    &transcript.as_srt(),
                );
                eprintln!(
                    "Saved live session transcript to {file_name}.{{txt,vtt,srt}}"
                );
            }
        }
        return Ok(());
    }

    let audio_str = args
        .audio
        .as_deref()
        .ok_or_else(|| anyhow!("Audio file path is required unless --live is specified."))?;
    let audio = Path::new(audio_str);
    let file_name = audio
        .file_name()
        .ok_or_else(|| anyhow!("Invalid audio path"))?
        .to_str()
        .ok_or_else(|| anyhow!("Non-UTF8 filename"))?;

    if !audio.exists() {
        return Err(anyhow!(
            "The provided audio file '{}' does not exist.",
            audio_str
        ));
    }

    let prepared_audio = whisper_cli::ffmpeg_decoder::prepare_audio(audio)?;
    let transcript = transcriber
        .transcribe(prepared_audio.path(), args.translate, args.karaoke)
        .await?;

    if args.stdout {
        println!("{}", transcript.as_text());
    }

    if !args.no_files && !args.stdout {
        write_to(
            audio.with_file_name(format!("{file_name}.txt")),
            &transcript.as_text(),
        );
        write_to(
            audio.with_file_name(format!("{file_name}.vtt")),
            &transcript.as_vtt(),
        );
        write_to(
            audio.with_file_name(format!("{file_name}.srt")),
            &transcript.as_srt(),
        );
        println!(
            "Success! Transcription completed in {:?}",
            transcript.processing_time
        );
    }

    Ok(())
}
