# Voice Agent

A real-time voice assistant built in Rust that captures audio from your microphone, transcribes speech using Gradium STT, processes it through an LLM (OpenAI/Groq compatible), and speaks the response using Gradium TTS.

## Features

- **Real-time speech-to-text** via Gradium STT WebSocket API
- **LLM integration** with streaming responses (OpenAI, Groq, or any OpenAI-compatible API)
- **Text-to-speech** via Gradium TTS WebSocket API
- **Conversation history** maintained across the session
- **Sentence-level streaming** to TTS for faster response times
- **Automatic reconnection** for STT/TTS on connection drops
- **WebSocket server** for remote access via browser
- **Web-based UI** with modern AudioWorklet for low-latency audio capture

## Architecture

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│  Microphone │────▶│  STT (ASR)  │────▶│     LLM     │────▶│     TTS     │
│  (48kHz)    │     │  (24kHz)    │     │  (streaming)│     │  (48kHz)    │
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
| `LLM_SYSTEM_PROMPT` | *"You are a helpful voice assistant..."* | System prompt for the LLM |
| `BIND_ADDR` | `127.0.0.1:8080` | WebSocket server bind address (for `voice-agent-ws`) |
| `GRADIUM_STT_ENDPOINT` | Gradium default | STT WebSocket endpoint |
| `GRADIUM_TTS_ENDPOINT` | Gradium default | TTS WebSocket endpoint |
| `GRADIUM_TTS_VOICE_ID` | Gradium default | TTS voice ID |
| `GRADIUM_STT_LANGUAGE` | `en` | STT language code |
| `RUST_LOG` | `info` | Log level (`debug`, `info`, `warn`, `error`) |

### Example: Using Groq

```bash
export LLM_API_KEY=gsk_xxxxx
export LLM_ENDPOINT=https://api.groq.com/openai/v1/chat/completions
export LLM_MODEL=llama-3.3-70b-versatile
```

## Running

### Option 1: Direct Audio I/O (Local)

Runs with direct microphone and speaker access:

```bash
# Set required environment variables
export GRADIUM_API_KEY=your_gradium_key
export OPENAI_API_KEY=your_openai_key

# Run the voice agent
cargo run --release --bin voice-agent

# Or with debug logging
RUST_LOG=debug cargo run --release --bin voice-agent
```

### Option 2: WebSocket Server (Remote Access)

Runs as a WebSocket server that can be accessed from a browser:

```bash
# Set required environment variables
export GRADIUM_API_KEY=your_gradium_key
export OPENAI_API_KEY=your_openai_key

# Optional: Set custom bind address (default: 127.0.0.1:8080)
export BIND_ADDR=127.0.0.1:8080

# Run the WebSocket server
cargo run --release --bin voice-agent-ws

# Or with debug logging
RUST_LOG=debug cargo run --release --bin voice-agent-ws
```

### Using the WebSocket Client UI

1. **Start the WebSocket server** (see Option 2 above)

2. **Serve the client files** using a local HTTP server:

   ```bash
   # Navigate to the ws directory
   cd src/ws
   
   # Option A: Using Python 3
   python3 -m http.server 8000
   
   # Option B: Using Python 2
   python -m SimpleHTTPServer 8000
   
   # Option C: Using Node.js (if you have npx)
   npx http-server -p 8000
   ```

3. **Open the UI** in your browser:
   - Navigate to `http://localhost:8000/ws.html`
   - The page will load the WebSocket client interface

4. **Connect and use**:
   - Enter the WebSocket URL (default: `ws://127.0.0.1:8080/ws`)
   - Click "Connect" to establish connection
   - Click "Start Recording" to begin voice interaction
   - Speak into your microphone
   - The agent will transcribe, process, and respond with audio

**Note**: The UI requires HTTP/HTTPS (not `file://`) because AudioWorklet needs to load modules. Use a local HTTP server as shown above.

### WebSocket Protocol

The WebSocket server accepts the following message types:

**Client → Server:**
- `{"type": "audio", "data": "<base64-encoded-pcm>"}` - Send audio chunks (24kHz, mono, i16 PCM)
- `{"type": "ping"}` - Heartbeat ping

**Server → Client:**
- `{"type": "audio", "data": "<base64-encoded-pcm>"}` - Receive audio chunks (48kHz, mono, i16 PCM)
- `{"type": "reset"}` - Reset playback (sent before new LLM responses)
- `{"type": "error", "message": "<error-text>"}` - Error message
- `{"type": "pong"}` - Heartbeat response

Each WebSocket connection creates its own VoiceAgent instance, so multiple clients can connect simultaneously.

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

### Direct Audio I/O Mode

1. Start the voice agent: `cargo run --release --bin voice-agent`
2. Wait for "voice agent started" message
3. Speak into your microphone
4. The agent will:
   - Transcribe your speech (STT)
   - Send transcription to LLM
   - Stream LLM response to TTS
   - Play audio response through speakers
5. Press `Ctrl+C` to stop

### WebSocket Mode

1. Start the WebSocket server: `cargo run --release --bin voice-agent-ws`
2. In another terminal, start a local HTTP server in `src/ws/` directory
3. Open `http://localhost:8000/ws.html` in your browser
4. Click "Connect" to establish WebSocket connection
5. Click "Start Recording" to begin voice interaction
6. Speak into your microphone - the agent will respond with audio
7. Click "Disconnect" when done

## Project Structure

```
voice-agent/
├── Cargo.toml
├── README.md
├── src/
│   ├── lib.rs               # Library exports
│   ├── voice_agent.rs       # Main orchestration logic
│   ├── llm.rs               # LLM client with streaming
│   ├── messages.rs          # Shared message types
│   ├── stt_handle.rs        # STT WebSocket wrapper
│   ├── tts_handle.rs        # TTS WebSocket wrapper
│   ├── local/
│   │   ├── main.rs          # Entry point for direct audio I/O mode
│   │   ├── pcm_capture.rs   # Audio input (microphone)
│   │   └── pcm_playback.rs  # Audio output (speakers)
│   └── ws/
│       ├── main.rs          # WebSocket server binary
│       ├── ws.rs            # WebSocket session actor
│       ├── ws.html          # WebSocket client UI
│       ├── ws.js            # WebSocket client JavaScript
│       └── audio-processor.js  # AudioWorklet processor
└── external/
    └── rust-gradium/        # Gradium API client library
```

### Binaries

- **`voice-agent`** (`src/local/main.rs`): Direct audio I/O mode - uses system microphone and speakers
- **`voice-agent-ws`** (`src/ws/main.rs`): WebSocket server mode - accepts connections from web clients

### WebSocket Client Files

- **`ws.html`**: Web-based UI for connecting to the WebSocket server
- **`ws.js`**: Client-side JavaScript handling WebSocket communication and audio
- **`audio-processor.js`**: AudioWorklet processor for low-latency audio capture in the browser

## Technical Details

### Audio Specifications

- **Input (Microphone)**: 24kHz, mono, 16-bit PCM
- **Output (Speaker)**: 48kHz, mono, 16-bit PCM
- **STT Processing**: 24kHz (resampled from device rate if needed)
- **TTS Output**: 48kHz (from Gradium API)

### WebSocket Server

- Each WebSocket connection creates an independent VoiceAgent instance
- Supports multiple concurrent connections
- Automatic heartbeat/ping mechanism (5-second intervals)
- Connection timeout: 10 seconds without ping response

### Browser Requirements

- Modern browser with WebSocket and AudioWorklet support
- Chrome 66+, Firefox 76+, Safari 14.1+
- HTTPS or localhost required for AudioWorklet (security restriction)
- Microphone permissions required

## Troubleshooting

### WebSocket Client Issues

**"Failed to load audio processor" error:**
- Ensure you're using HTTP/HTTPS, not `file://` protocol
- Use a local HTTP server (see "Using the WebSocket Client UI" section)
- Check browser console for detailed error messages

**Audio not playing:**
- Check browser audio permissions
- Verify WebSocket connection is established (status should show "Connected")
- Check browser console for errors

**Connection refused:**
- Ensure WebSocket server is running (`voice-agent-ws`)
- Verify `BIND_ADDR` matches the URL in the client
- Check firewall settings

## License

MIT

