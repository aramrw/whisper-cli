use crate::{
    provider::Transcriber,
    transcript::{Transcript, Utternace},
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::{
    multipart::{Form, Part},
    Client,
};
use serde::Deserialize;
use std::{path::Path, time::Instant};
use tokio::fs::File;
use tokio_util::codec::{BytesCodec, FramedRead};

#[derive(Debug, Deserialize)]
struct CustomSegment {
    start: f64,
    end: f64,
    text: String,
}

#[derive(Debug, Deserialize)]
struct CustomWord {
    word: String,
    start: f64,
    end: f64,
}

#[derive(Debug, Deserialize)]
struct CustomVerboseResponse {
    #[serde(default)]
    segments: Vec<CustomSegment>,
    #[serde(default)]
    words: Vec<CustomWord>,
    #[serde(default)]
    text: String,
}

pub struct CustomApiProvider {
    client: Client,
    api_url: String,
    api_key: Option<String>,
    model: String,
    lang: Option<String>,
}

impl CustomApiProvider {
    pub fn new(
        api_url: String,
        api_key: Option<String>,
        model: Option<String>,
        lang: Option<String>,
    ) -> Self {
        Self {
            client: Client::new(),
            api_url,
            api_key,
            model: model.unwrap_or_else(|| "whisper-1".to_string()),
            lang,
        }
    }
}

#[async_trait]
impl Transcriber for CustomApiProvider {
    async fn transcribe(
        &self,
        audio_path: &Path,
        translate: bool,
        word_timestamps: bool,
    ) -> Result<Transcript> {
        let file = File::open(audio_path).await?;
        let file_name = audio_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("audio.wav")
            .to_string();

        let stream = FramedRead::new(file, BytesCodec::new());
        let file_part = Part::stream(reqwest::Body::wrap_stream(stream)).file_name(file_name);

        let endpoint_suffix = if translate {
            "/audio/translations"
        } else {
            "/audio/transcriptions"
        };
        let mut endpoint = self.api_url.trim_end_matches('/').to_string();
        if !endpoint.ends_with("/v1") && !endpoint.ends_with("/audio/transcriptions") {
            endpoint.push_str("/v1");
        }
        if !endpoint.ends_with("/audio/transcriptions") && !endpoint.ends_with("/audio/translations") {
            endpoint.push_str(endpoint_suffix);
        }

        let mut form = Form::new()
            .part("file", file_part)
            .text("model", self.model.clone())
            .text("response_format", "verbose_json");

        if let Some(lang) = &self.lang {
            form = form.text("language", lang.clone());
        }

        if word_timestamps {
            form = form.text("timestamp_granularities[]", "word");
        }

        let mut req = self.client.post(&endpoint).multipart(form);
        if let Some(key) = &self.api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }

        let st = Instant::now();
        let res = req.send().await?;

        if !res.status().is_success() {
            let err_text = res.text().await.unwrap_or_default();
            return Err(anyhow!("Custom API error: {}", err_text));
        }

        let response: CustomVerboseResponse = res.json().await?;

        let utterances = response
            .segments
            .into_iter()
            .map(|s| Utternace {
                start: (s.start * 100.0) as i64,
                stop: (s.end * 100.0) as i64,
                text: s.text,
            })
            .collect();

        let word_utterances = if word_timestamps && !response.words.is_empty() {
            Some(
                response
                    .words
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
