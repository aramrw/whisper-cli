#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use anyhow::{anyhow, Result};
use clap::{Parser, ValueEnum};
use std::{env, path::Path};
use utils::write_to;

mod utils;

use whisper_cli::{
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
    about = "Transcribe audio files using modern Speech-to-Text APIs.",
    long_about = "Generate a transcript of an audio file using Groq, OpenAI, Deepgram, or a custom OpenAI-compatible API endpoint. The transcript will be saved as a .txt, .vtt, and .srt file in the same directory as the audio file."
)]
struct Args {
    /// Speech-to-Text provider to use (groq, openai, deepgram, custom)
    #[clap(short, long, value_enum, default_value = "groq")]
    provider: ProviderKind,

    /// Name of the model to use (e.g. whisper-large-v3-turbo, whisper-1, nova-3)
    #[clap(short, long)]
    model: Option<String>,

    /// Language spoken in the audio (ISO 639-1 code e.g. 'en', 'es', 'de'). Auto-detected by default.
    #[clap(short = 'l', long)]
    lang: Option<String>,

    /// Path to the audio file to transcribe
    audio: String,

    /// API Key for the chosen provider (defaults to GROQ_API_KEY, OPENAI_API_KEY, or DEEPGRAM_API_KEY env var)
    #[clap(short = 'k', long)]
    api_key: Option<String>,

    /// Custom API Base URL for local/self-hosted OpenAI-compatible speech-to-text endpoints (e.g. http://localhost:8000/v1)
    #[clap(short = 'u', long)]
    api_url: Option<String>,

    /// Toggle translation to English
    #[clap(short, long, default_value = "false")]
    translate: bool,

    /// Generate timestamps for each word
    #[clap(long, default_value = "false")]
    karaoke: bool,
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
            let url = args.api_url.clone().ok_or_else(|| {
                anyhow!("Custom provider requires --api-url (e.g. http://localhost:8000/v1).")
            })?;
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
    let audio = Path::new(&args.audio);
    let file_name = audio
        .file_name()
        .ok_or_else(|| anyhow!("Invalid audio path"))?
        .to_str()
        .ok_or_else(|| anyhow!("Non-UTF8 filename"))?;

    if !audio.exists() {
        return Err(anyhow!("The provided audio file '{}' does not exist.", args.audio));
    }

    let transcriber = create_transcriber(&args)?;
    let transcript = transcriber
        .transcribe(audio, args.translate, args.karaoke)
        .await?;

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

    println!("Success! Transcription completed in {:?}", transcript.processing_time);
    Ok(())
}
