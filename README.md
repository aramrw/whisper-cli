# whisper-cli

A high-performance command line tool in Rust for transcribing audio files using modern, ultra-fast Speech-to-Text APIs (Groq, OpenAI, Deepgram) and self-hosted local OpenAI-compatible endpoints.

Generates transcriptions in `.txt`, `.vtt`, and `.srt` formats automatically.

---

## Features

- ⚡ **Ultra-Fast Transcription**: Uses Groq (`whisper-large-v3-turbo`), OpenAI (`whisper-1`), Deepgram (`nova-3`), or custom endpoints.
- 🚀 **Zero Local C++/CMake Build Hassle**: No native C++ building or multi-gigabyte model downloads required.
- 🌐 **Multi-Provider Support**: Switch easily between Groq, OpenAI, Deepgram, or local/self-hosted servers (Faster-Whisper, Ollama, LocalAI, vLLM).
- 📝 **Multiple Output Formats**: Saves `.txt`, `.vtt`, and `.srt` subtitle files in the same directory as the input audio.
- ⏱️ **Word-level Timestamps**: Generate karaoke-style word timestamps with `--karaoke`.
- 🌍 **Translation**: Translate non-English audio directly to English with `-t / --translate`.

---

## Installation & Build

Ensure you have Rust installed ([rustup.rs](https://rustup.rs/)).

```bash
git clone https://github.com/aramrw/whisper-cli && cd whisper-cli
cargo build --release
```

Install binary system-wide (optional):
```bash
cp target/release/whisper /usr/local/bin/whisper
```

---

## API Key Setup

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
whisper audio.mp3
```

### 2. Transcribe with OpenAI or Deepgram
```bash
whisper --provider openai --model whisper-1 audio.mp3
whisper --provider deepgram --model nova-3 audio.mp3
```

### 3. Translate Audio to English
```bash
whisper --translate audio.mp3
```

### 4. Generate Word-Level Subtitles (Karaoke mode)
```bash
whisper --karaoke audio.mp3
```

### 5. Use a Local / Self-Hosted Endpoint (Ollama / Faster-Whisper / LocalAI)
```bash
whisper --provider custom --api-url http://localhost:8000/v1 audio.wav
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
  -h, --help                 Print help
  -V, --version              Print version
```

---

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
