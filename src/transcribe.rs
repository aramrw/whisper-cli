// transcribe.rs
use crate::{
    ffmpeg_decoder,
    transcript::{Transcript, Utternace},
    Whisper,
};
use anyhow::{anyhow, Result};
use std::{path::Path, time::Instant};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperTokenData,
};

impl Whisper {
    pub fn transcribe<P: AsRef<Path>>(
        &mut self,
        audio: P,
        translate: bool,
        word_timestamps: bool,
    ) -> Result<Transcript> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });

        params.set_translate(translate);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // Note: The new API integrates token timestamps differently.
        // We enable it by iterating over tokens later.
        // params.set_token_timestamps(word_timestamps); // This function may have been removed or changed.

        // This part should work if ffmpeg_decoder::read_file returns Vec<f32>
        let audio = ffmpeg_decoder::read_file(audio)?;

        let st = Instant::now();
        let mut state = self.ctx.create_state().expect("failed to create state");
        state.full(params, &audio).expect("failed to transcribe");

        // --- START OF MIGRATED CODE ---

        let mut words = Vec::new();
        let mut utterances = Vec::new();

        // The biggest change: Use an iterator instead of manual indexing.
        for segment in state.as_iter() {
            // Get segment data directly from the `segment` object.
            let text = segment.to_string();
            let start = segment.start_timestamp();
            let stop = segment.end_timestamp();

            utterances.push(Utternace { text, start, stop });

            if word_timestamps {
                // number of tokens in this segment.
                let num_tokens = segment.n_tokens();

                // loop through every token
                for t_index in 0..num_tokens {
                    if let Some(token_data) = segment.get_token(t_index) {
                        // hardcoding type (this compiles);
                        let whisper_token: whisper_rs::WhisperToken<'_, '_> = token_data;
                        let text = whisper_token.to_string();

                        if text.starts_with("[_") {
                            continue;
                        }

                        let WhisperTokenData { t0, t1, .. } = whisper_token.token_data();

                        words.push(Utternace {
                            text,
                            start: t0,
                            stop: t1,
                        });
                    }
                }
            }
        }

        if utterances.is_empty() {
            return Err(anyhow!("No segments found"));
        }

        Ok(Transcript {
            utterances,
            processing_time: Instant::now().duration_since(st),
            word_utterances: if word_timestamps { Some(words) } else { None },
        })
    }
}
