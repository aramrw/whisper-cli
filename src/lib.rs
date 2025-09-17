mod ffmpeg_decoder;
mod model;
mod transcribe;
mod transcript;
mod utils;
mod whisper;

use std::sync::LazyLock;

pub use clap::*;
pub use model::{Model, Size};
pub use transcript::{Transcript, Utternace};
pub use whisper::{Language, Whisper};

#[derive(Parser)]
#[command(
    author,
    version,
    about = "Locally transcribe audio files, using Whisper.",
    long_about = "Generate a transcript of an audio file using the Whisper speech-to-text engine. The transcript will be saved as a .txt, .vtt, and .srt file in the same directory as the audio file."
)]
pub struct Args {
    /// Name of the Whisper model to use
    #[clap(short, long, default_value = "medium")]
    pub model: Size,

    /// Language spoken in the audio. Attempts to auto-detect by default.
    #[clap(short, long)]
    pub lang: Option<Language>,

    /// Path to the audio file to transcribe
    pub audio: String,

    /// Toggle translation
    #[clap(short, long, default_value = "false")]
    pub translate: bool,

    /// Generate timestamps for each word
    #[clap(short, long, default_value = "false")]
    pub karaoke: bool,

    /// Strips unecessary audio like music and silence
    #[clap(long, default_value = "false")]
    pub auto_strip: bool,
}

pub static CLI: LazyLock<Args> = LazyLock::new(|| {
    let mut args = Args::parse();

    if args.model.is_english_only() && (args.lang == Some(Language::Auto) || args.lang.is_none()) {
        args.lang = Some(Language::English);
    }
    args
});
