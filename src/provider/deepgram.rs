use crate::{
    provider::Transcriber,
    transcript::{Transcript, Utternace},
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use std::{path::Path, time::Instant};
use tokio::fs;

#[derive(Debug, Deserialize)]
struct DeepgramWord {
    word: String,
    start: f64,
    end: f64,
}

#[derive(Debug, Deserialize)]
struct DeepgramUtterance {
    transcript: String,
    start: f64,
    end: f64,
}

#[derive(Debug, Deserialize)]
struct DeepgramAlternative {
    #[serde(default)]
    words: Vec<DeepgramWord>,
}

#[derive(Debug, Deserialize)]
struct DeepgramChannel {
    #[serde(default)]
    alternatives: Vec<DeepgramAlternative>,
}

#[derive(Debug, Deserialize)]
struct DeepgramResults {
    #[serde(default)]
    channels: Vec<DeepgramChannel>,
    #[serde(default)]
    utterances: Vec<DeepgramUtterance>,
}

#[derive(Debug, Deserialize)]
struct DeepgramResponse {
    results: Option<DeepgramResults>,
}

pub struct DeepgramProvider {
    client: Client,
    api_key: String,
    model: String,
    lang: Option<String>,
}

impl DeepgramProvider {
    pub fn new(api_key: String, model: Option<String>, lang: Option<String>) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model: model.unwrap_or_else(|| "nova-3".to_string()),
            lang,
        }
    }
}

#[async_trait]
impl Transcriber for DeepgramProvider {
    async fn transcribe(
        &self,
        audio_path: &Path,
        _translate: bool,
        word_timestamps: bool,
    ) -> Result<Transcript> {
        let audio_bytes = fs::read(audio_path).await?;

        let mut url = format!(
            "https://api.deepgram.com/v1/listen?model={}&punctuate=true&utterances=true",
            self.model
        );
        if let Some(lang) = &self.lang {
            url.push_str(&format!("&language={lang}"));
        }

        let st = Instant::now();
        let res = self
            .client
            .post(&url)
            .header("Authorization", format!("Token {}", self.api_key))
            .header("Content-Type", "audio/*")
            .body(audio_bytes)
            .send()
            .await?;

        if !res.status().is_success() {
            let err_text = res.text().await.unwrap_or_default();
            return Err(anyhow!("Deepgram API error: {}", err_text));
        }

        let response: DeepgramResponse = res.json().await?;

        let results = response
            .results
            .ok_or_else(|| anyhow!("No results returned from Deepgram"))?;

        let utterances = results
            .utterances
            .into_iter()
            .map(|u| Utternace {
                start: (u.start * 100.0) as i64,
                stop: (u.end * 100.0) as i64,
                text: u.transcript,
            })
            .collect();

        let words = results
            .channels
            .into_iter()
            .next()
            .and_then(|c| c.alternatives.into_iter().next())
            .map(|a| a.words)
            .unwrap_or_default();

        let word_utterances = if word_timestamps && !words.is_empty() {
            Some(
                words
                    .into_iter()
                    .map(|w| Utternace {
                        start: (w.start * 100.0) as i64,
                        stop: (w.end * 100.0) as i64,
                        text: w.word,
                    })
                    .collect(),
            )
        } else {
            None
        };

        Ok(Transcript {
            processing_time: Instant::now().duration_since(st),
            utterances,
            word_utterances,
        })
    }
}
