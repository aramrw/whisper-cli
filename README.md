# whisper-cli

A high-performance command line tool in Rust for transcribing audio and video files using modern Speech-to-Text APIs (Groq, OpenAI, Deepgram) and self-hosted local OpenAI-compatible endpoints (`whisper-server`, Faster-Whisper, Ollama, LocalAI, vLLM).

Generates transcriptions in `.txt`, `.vtt`, and `.srt` formats automatically.

---

## Features

- ⚡ **Ultra-Fast Cloud & Local Transcription**: Uses Groq (`whisper-large-v3-turbo`), OpenAI (`whisper-1`), Deepgram (`nova-3`), or local GPU-accelerated endpoints.
- 🎬 **Universal Audio & Video Support**: Automatic `ffmpeg` pre-decoding for `.mp3`, `.mp4`, `.m4a`, `.wav`, `.mkv`, `.mov`, `.flac`, `.ogg`, and `.webm`.
- 🌐 **Multi-Provider Architecture**: Switch easily between Groq, OpenAI, Deepgram, or local/self-hosted servers.
- 📝 **Multiple Output Formats**: Automatically writes `.txt`, `.vtt`, and `.srt` subtitle files in the same directory as the input file.
- 📺 **Terminal Output Mode**: Use `-s` / `--stdout` to output transcript text directly to standard output without saving files.
- ⏱️ **Word-level Timestamps**: Generate karaoke-style word timestamps with `--karaoke`.
- 🌍 **Language Selection & Translation**: Explicit language specification (`-l es`, `-l en`, `-l ja`) and direct translation to English (`-t / --translate`).

---

## Installation & Build

Ensure you have Rust installed ([rustup.rs](https://rustup.rs/)).

```bash
git clone https://github.com/aramrw/whisper-cli && cd whisper-cli
cargo build --release
```

Install binary system-wide:
```bash
cargo install --path .
```

---

## Setting Up a Local Server (`whisper-cpp` on macOS)

To run transcriptions 100% locally with Apple Silicon Metal GPU acceleration without sending data to cloud APIs:

### 1. Install `whisper-cpp`
```bash
brew install whisper-cpp
```

### 2. Download a model file
* **Medium** (Recommended for speed & high accuracy):
  ```bash
  mkdir -p ~/.cache/whisper-models
  curl -L -o ~/.cache/whisper-models/ggml-medium.bin "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.bin"
  ```
* **Large v3 Turbo**:
  ```bash
  curl -L -o ~/.cache/whisper-models/ggml-large-v3-turbo.bin "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin"
  ```

### 3. Start the local server (Anti-Glitch & Fast Flags)
Launch `whisper-server` with Metal GPU Flash Attention (`-fa`), CPU threads (`-t 8`), no-speech threshold (`-nth 0.6`), zero context memory (`-mc 0`), and non-speech token suppression (`-sns`):

```bash
whisper-server -m ~/.cache/whisper-models/ggml-large-v3-turbo.bin --port 8080 -t 8 -fa -nth 0.6 -mc 0 -sns
```

> **Server Configuration Notes (Preventing Repetition & Glitches):**
> - `-fa`: Enables Flash Attention on Apple Metal GPU.
> - `-t 8`: Utilizes 8 CPU performance threads for pre/post-processing.
> - `-nth 0.6`: Sets `no_speech_threshold` to `0.6` to filter out background noise/silence hallucinations.
> - `-mc 0`: Sets `max-context` to `0` to prevent repeating text loop glitches across 30-second window boundaries.
> - `-sns`: Enables `suppress-nst` to prevent non-speech token hallucinations (like `[Music]`, `[Laughter]`, `(cheering)`).

### 4. Transcribe with `whisper-cli`
```bash
whisper -p custom -u http://localhost:8080 audio.mp3
```

---

## API Key Setup (Cloud Providers)

Set the environment variable for your preferred provider:

* **Groq** (Default - recommended for speed & free tier):
  ```bash
  export GROQ_API_KEY="gsk_..."
  ```
* **OpenAI**:
  ```bash
  export OPENAI_API_KEY="sk-..."
  ```
* **Deepgram**:
  ```bash
  export DEEPGRAM_API_KEY="..."
  ```

Alternatively, pass `-k / --api-key` directly on the command line.

---

## Usage Examples

### 1. Transcribe with Groq (Default)
```bash
whisper video.mp4
```

### 2. Specify Language (e.g. Spanish, Japanese, English)
```bash
whisper -l es spanish_audio.mp3
whisper -l ja anime_clip.mp4
whisper -l en podcast.m4a
```

### 3. Generate `.srt`, `.vtt`, and `.txt` Files locally via `whisper-server`
```bash
whisper -p custom -u http://localhost:8080 -l es ~/Downloads/video.mp4
```

### 4. Print Directly to Terminal Stdout (No Files Created)
```bash
whisper -s -p custom -u http://localhost:8080 audio.mp3
```

### 5. Generate Word-Level Subtitles (Karaoke mode)
```bash
whisper --karaoke video.mp4
```

### 6. Translate Audio to English
```bash
whisper --translate video.mp4
```

---

## CLI Options (`whisper --help`)

```text
Generate a transcript of an audio file using Groq, OpenAI, Deepgram, or a custom OpenAI-compatible API endpoint. The transcript will be saved as a .txt, .vtt, and .srt file in the same directory as the audio file.

Usage: whisper [OPTIONS] <AUDIO>

Arguments:
  <AUDIO>  Path to the audio file to transcribe

Options:
  -p, --provider <PROVIDER>  Speech-to-Text provider to use (groq, openai, deepgram, custom) [default: groq]
  -m, --model <MODEL>        Name of the model to use (e.g. whisper-large-v3-turbo, whisper-1, nova-3)
  -l, --lang <LANG>          Language spoken in the audio (ISO 639-1 code e.g. 'en', 'es', 'de')
  -k, --api-key <API_KEY>    API Key (defaults to GROQ_API_KEY, OPENAI_API_KEY, or DEEPGRAM_API_KEY env var)
  -u, --api-url <API_URL>    Custom API Base URL for local/self-hosted OpenAI-compatible speech-to-text endpoints
  -t, --translate            Toggle translation to English
      --karaoke              Generate timestamps for each word
  -s, --stdout               Print transcript directly to terminal stdout
      --no-files             Skip writing .txt, .vtt, and .srt files to disk
  -h, --help                 Print help
  -V, --version              Print version
```

---

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
