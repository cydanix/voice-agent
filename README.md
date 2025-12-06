# Voice Agent

A real-time voice assistant built in Rust that captures audio from your microphone, transcribes speech using Gradium STT, processes it through an LLM (OpenAI/Groq compatible), and speaks the response using Gradium TTS.

## Features

- **Real-time speech-to-text** via Gradium STT WebSocket API
- **LLM integration** with streaming responses (OpenAI, Groq, or any OpenAI-compatible API)
- **Text-to-speech** via Gradium TTS WebSocket API
- **Conversation history** maintained across the session
- **Sentence-level streaming** to TTS for faster response times
- **Automatic reconnection** for STT/TTS on connection drops

## Architecture

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│  Microphone │────▶│  STT (ASR)  │────▶│     LLM     │────▶│     TTS     │
│  (48kHz)    │     │  (24kHz)    │     │  (streaming)│     │  (24kHz)    │
└─────────────┘     └─────────────┘     └─────────────┘     └─────────────┘
                                                                   │
                                                                   ▼
                                                            ┌─────────────┐
                                                            │   Speaker   │
                                                            │  (48kHz)    │
                                                            └─────────────┘
```

## Prerequisites

- **Rust** 1.75+ (install via [rustup](https://rustup.rs/))
- **macOS** (tested on macOS, uses CoreAudio via cpal)
- **Gradium API Key** for STT/TTS
- **OpenAI API Key** (or Groq/compatible API key) for LLM

## Building

```bash
# Clone the repository
git clone <repo-url>
cd voice-agent

# Build in release mode
cargo build --release

# Or build in debug mode for development
cargo build
```

## Environment Variables

### Required

| Variable | Description |
|----------|-------------|
| `GRADIUM_API_KEY` | API key for Gradium STT/TTS services |
| `OPENAI_API_KEY` | API key for OpenAI (or use `LLM_API_KEY` for other providers) |

### Optional

| Variable | Default | Description |
|----------|---------|-------------|
| `LLM_API_KEY` | - | Alternative to `OPENAI_API_KEY` |
| `OPENAI_MODEL` | `gpt-4o-mini` | LLM model to use |
| `LLM_MODEL` | - | Alternative to `OPENAI_MODEL` |
| `LLM_ENDPOINT` | `https://api.openai.com/v1/chat/completions` | LLM API endpoint |
| `SYSTEM_PROMPT` | *"You are a helpful voice assistant..."* | System prompt for the LLM |
| `RUST_LOG` | `info` | Log level (`debug`, `info`, `warn`, `error`) |

### Example: Using Groq

```bash
export LLM_API_KEY=gsk_xxxxx
export LLM_ENDPOINT=https://api.groq.com/openai/v1/chat/completions
export LLM_MODEL=llama-3.3-70b-versatile
```

## Running

```bash
# Set required environment variables
export GRADIUM_API_KEY=your_gradium_key
export OPENAI_API_KEY=your_openai_key

# Run the voice agent
cargo run --release

# Or with debug logging
RUST_LOG=debug cargo run --release
```

## macOS Permissions

### Microphone Access

When you first run the application, macOS will prompt you to allow microphone access. If you denied it or need to re-enable:

1. Open **System Preferences** → **Privacy & Security** → **Privacy**
2. Select **Microphone** from the left sidebar
3. Find your terminal app (Terminal, iTerm2, etc.) and enable it
4. Restart your terminal

Alternatively, via command line:
```bash
# Check current microphone permissions
tccutil reset Microphone
```

### Audio Output

Audio output typically works without additional permissions. If you experience issues:

1. Open **System Preferences** → **Sound** → **Output**
2. Ensure the correct output device is selected
3. Check that the volume is not muted

### Troubleshooting Permissions

If the app can't access the microphone:

```bash
# Reset microphone permissions for Terminal
tccutil reset Microphone com.apple.Terminal

# For iTerm2
tccutil reset Microphone com.googlecode.iterm2
```

Then run the app again and approve the permission prompt.

## Usage

1. Start the voice agent
2. Wait for "voice agent started" message
3. Speak into your microphone
4. The agent will:
   - Transcribe your speech (STT)
   - Send transcription to LLM
   - Stream LLM response to TTS
   - Play audio response through speakers
5. Press `Ctrl+C` to stop

## Project Structure

```
voice-agent/
├── Cargo.toml
├── README.md
├── src/
│   ├── main.rs           # Entry point, config loading
│   ├── voice_agent.rs    # Main orchestration
│   ├── llm.rs            # LLM client with streaming
│   ├── stt_handle.rs     # STT WebSocket wrapper
│   ├── tts_handle.rs     # TTS WebSocket wrapper
│   ├── pcm_capture.rs    # Audio input (microphone)
│   └── pcm_playback.rs   # Audio output (speakers)
└── external/
    └── rust-gradium/     # Gradium API client library
```

## License

MIT

