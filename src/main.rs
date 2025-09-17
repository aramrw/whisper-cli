#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::path::Path;
use utils::write_to;

mod utils;

use whisper_cli::{CLI, Language, Model, Whisper};

#[tokio::main]
async fn main() {
    let args = &CLI;
    let audio = Path::new(&args.audio);
    let file_name = audio.file_name().unwrap().to_str().unwrap();

    assert!(audio.exists(), "The provided audio file does not exist.");

    assert!(
        !args.model.is_english_only() || args.lang == Some(Language::English),
        "The selected model only supports English."
    );
    let mut whisper = Whisper::new(Model::new(args.model), args.lang).await;
    let transcript = whisper
        .transcribe(audio, args.translate, args.karaoke)
        .expect("failed to transcribe audio");

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

    println!("time: {:?}", transcript.processing_time);
}
