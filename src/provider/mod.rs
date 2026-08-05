pub mod custom;
pub mod deepgram;
pub mod groq;
pub mod openai;

use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

use crate::transcript::Transcript;

#[async_trait]
pub trait Transcriber: Send + Sync {
    async fn transcribe(
        &self,
        audio_path: &Path,
        translate: bool,
        word_timestamps: bool,
    ) -> Result<Transcript>;
}
