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

## Customizing Behavior with Event Handlers

The voice agent supports custom event handlers that allow you to observe and modify agent behavior. This is useful for:
- **Logging and monitoring** - Track all events in the voice agent lifecycle
- **Input preprocessing** - Modify user input before sending to the LLM
- **Response filtering** - Monitor or filter LLM responses
- **TTS monitoring** - Observe what text is being spoken
- **Error handling** - Custom error handling and recovery

### Implementing a Custom Event Handler

Create a struct that implements the `VoiceAgentEventHandler` trait:

```rust
use voice_agent::voice_agent::{VoiceAgentEventHandler, VoiceAgent};
use async_trait::async_trait;
use std::sync::Arc;

#[derive(Clone)]
struct MyEventHandler {
    // Add any state you need
}

#[async_trait]
impl VoiceAgentEventHandler for MyEventHandler {
    // Modify user input before sending to LLM
    async fn on_user_input(&self, input: String) -> String {
        println!("User said: {}", input);
        // You can modify the input here
        // For example, add a prefix or filter certain words
        format!("[Modified] {}", input)
    }

    // Observe when user interrupts (e.g., says "stop")
    async fn on_user_break(&self, text: String) {
        println!("User interrupted with: {}", text);
    }

    // Monitor TTS speech output
    async fn on_tts_speech(&self, text: String) {
        println!("Speaking: {}", text);
        // Note: Currently this is observation-only
        // The original text is still used for TTS processing
    }

    // Handle TTS errors
    async fn on_tts_error(&self, error: String) {
        eprintln!("TTS error: {}", error);
    }

    // Handle STT errors
    async fn on_stt_error(&self, error: String) {
        eprintln!("STT error: {}", error);
    }

    // Handle TTS connection close or end-of-speech
    async fn on_tts_close_or_eos(&self) {
        println!("TTS connection closed or end of speech");
    }

    // Handle STT connection close or end-of-speech
    async fn on_stt_close_or_eos(&self) {
        println!("STT connection closed or end of speech");
    }

    // Handle general errors
    async fn on_error(&self, error_message: String) {
        eprintln!("Voice agent error: {}", error_message);
    }

    // Monitor LLM response chunks (streaming)
    async fn on_llm_response_chunk(&self, text: String) {
        println!("LLM chunk: {}", text);
    }

    // Monitor complete LLM response
    async fn on_llm_response_done(&self, text: String) {
        println!("LLM response complete: {}", text);
    }

    // Handle shutdown
    async fn on_shutdown(&self) {
        println!("Voice agent shutting down");
    }
}

// Usage:
let event_handler = Arc::new(MyEventHandler {});
agent.start(capture_rx, playback_tx, event_handler).await?;
```

### Event Handler Methods

| Method | Purpose | Can Modify? |
|--------|---------|-------------|
| `on_user_input` | Called when user speech is transcribed | ✅ Yes - returns modified input |
| `on_user_break` | Called when user interrupts (e.g., "stop") | ❌ No - observation only |
| `on_tts_speech` | Called when text is sent to TTS | ❌ No - observation only |
| `on_tts_error` | Called on TTS errors | ❌ No - observation only |
| `on_stt_error` | Called on STT errors | ❌ No - observation only |
| `on_tts_close_or_eos` | Called when TTS connection closes or ends | ❌ No - observation only |
| `on_stt_close_or_eos` | Called when STT connection closes or ends | ❌ No - observation only |
| `on_error` | Called on general errors (takes error message) | ❌ No - observation only |
| `on_llm_response_chunk` | Called for each streaming LLM chunk | ❌ No - observation only |
| `on_llm_response_done` | Called when LLM response is complete | ❌ No - observation only |
| `on_shutdown` | Called during agent shutdown | ❌ No - observation only |

### Example: Input Preprocessing

Modify user input before it's sent to the LLM:

```rust
async fn on_user_input(&self, input: String) -> String {
    // Add context or modify input
    if input.to_lowercase().contains("weather") {
        format!("What is the weather today? User also said: {}", input)
    } else {
        input
    }
}
```

### Example: Logging Handler

Create a comprehensive logging handler:

```rust
use tracing::{info, error, warn};

struct LoggingEventHandler;

#[async_trait]
impl VoiceAgentEventHandler for LoggingEventHandler {
    async fn on_user_input(&self, input: String) -> String {
        info!("User input: {}", input);
        input
    }

    async fn on_llm_response_done(&self, text: String) {
        info!("LLM response: {}", text);
    }

    async fn on_tts_speech(&self, text: String) {
        info!("TTS speaking: {}", text);
    }

    // ... implement other methods
}
```

### Injecting TTS Speech Programmatically

You can programmatically inject text to be spoken by the agent using the `inject_tts_speech` method. This is useful for:
- **Greetings** - Say hello when the agent starts
- **Notifications** - Alert the user about events
- **Custom interactions** - Create your own voice responses outside of the LLM flow
- **System messages** - Provide status updates or instructions

```rust
use voice_agent::voice_agent::{VoiceAgent, Config, VoiceAgentNoOpEventHandler};
use std::sync::Arc;

// After starting the agent
let mut agent = VoiceAgent::new(config);
let event_handler = Arc::new(VoiceAgentNoOpEventHandler);
agent.start(capture_rx, playback_tx, event_handler).await?;

// Inject speech programmatically
agent.inject_tts_speech("Hello! I'm ready to help.".to_string());
agent.inject_tts_speech("What would you like to know?".to_string());

// The text will be spoken immediately through TTS
// This bypasses the LLM and goes directly to TTS processing
```

**Important Notes:**
- `inject_tts_speech` is **non-blocking** - it queues the text for TTS processing
- The text will trigger the `on_tts_speech` event handler callback
- Multiple calls will queue the text in order
- The agent must be started (via `start()`) before calling this method
- This method is thread-safe and can be called from any thread

### Example: Interactive Greeting

```rust
// Start the agent
agent.start(capture_rx, playback_tx, event_handler).await?;
info!("Voice agent started");

// Greet the user immediately
agent.inject_tts_speech("Hello! I'm your voice assistant. How can I help you today?".to_string());

// Continue with normal operation
agent.run().await?;
```

### Injecting Errors from Upper Layers

You can inject errors from upper layers (your application code) using the `inject_error` method. This is useful for:
- **Application-level errors** - Report errors from your business logic
- **External service failures** - Notify about failures in external dependencies
- **Validation errors** - Report validation failures
- **Graceful shutdown triggers** - Signal that the agent should stop due to an error condition

```rust
use voice_agent::voice_agent::{VoiceAgent, Config, VoiceAgentNoOpEventHandler};
use std::sync::Arc;

// After starting the agent
let mut agent = VoiceAgent::new(config);
let event_handler = Arc::new(VoiceAgentNoOpEventHandler);
agent.start(capture_rx, playback_tx, event_handler).await?;

// Inject an error from your application layer
if some_condition_failed {
    agent.inject_error("Failed to connect to external service".to_string());
}

// The error will:
// 1. Trigger the on_error event handler callback
// 2. Log the error
// 3. Stop the agent's main loop gracefully
```

**Important Notes:**
- `inject_error` is **non-blocking** - it sends the error event asynchronously
- The error will trigger the `on_error` event handler callback with the error message
- **The agent will stop** after processing the error (the main loop breaks)
- The agent must be started (via `start()`) before calling this method
- This method is thread-safe and can be called from any thread
- Use this for fatal errors that require the agent to stop

### Example: Error Handling with Custom Event Handler

```rust
struct ErrorHandlingEventHandler;

#[async_trait]
impl VoiceAgentEventHandler for ErrorHandlingEventHandler {
    // ... other methods ...

    async fn on_error(&self, error_message: String) {
        eprintln!("Agent error: {}", error_message);
        // Perform cleanup, notify user, etc.
        // The agent will stop after this callback completes
    }
}

// In your application code
if external_service_failed {
    agent.inject_error(format!("External service unavailable: {}", service_name));
    // Agent will stop gracefully after on_error is called
}
```

### No-Op Handler

For basic usage without customization, use `VoiceAgentNoOpEventHandler`:

```rust
use voice_agent::voice_agent::VoiceAgentNoOpEventHandler;

let event_handler = Arc::new(VoiceAgentNoOpEventHandler);
```

## Managing LLM Conversation History

The voice agent maintains conversation history automatically, but you can also manage it programmatically. This is useful for:
- **Context management** - Load previous conversations or clear history
- **System prompt customization** - Change the assistant's behavior dynamically
- **Multi-session support** - Save and restore conversation state
- **Custom history management** - Implement your own history persistence

### Getting and Setting History via VoiceAgent

The recommended way to manage history is through the `VoiceAgent` methods:

```rust
use voice_agent::voice_agent::{VoiceAgent, Config, VoiceAgentNoOpEventHandler};
use voice_agent::llm::{LlmHistory, Message};
use std::sync::Arc;

let mut agent = VoiceAgent::new(config);
let event_handler = Arc::new(VoiceAgentNoOpEventHandler);
agent.start(capture_rx, playback_tx, event_handler).await?;

// Get current conversation history
let history: LlmHistory = agent.get_llm_history().await;

// Get messages from history
let messages = history.get_messages();
for message in messages {
    println!("{:?}: {}", message.role, message.content);
}

// Set a new history (useful for loading saved conversations)
let mut new_history = LlmHistory::new();
new_history.set_system_prompt("You are a technical assistant.".to_string());
new_history.add_message(Message::user("Previous context"));
agent.set_llm_history(new_history).await;

// Clear history (resets to default system prompt)
agent.clear_llm_history().await;
```

### Managing System Prompt via VoiceAgent

The system prompt defines the assistant's behavior and personality. You can get and update it at runtime:

```rust
// Get current system prompt
let current_prompt = agent.get_llm_system_prompt().await;
println!("Current system prompt: {}", current_prompt);

// Update system prompt dynamically
agent.set_llm_system_prompt("You are a friendly customer service agent.".to_string()).await;

// The next LLM request will use the new system prompt
```

### Example: Dynamic System Prompt Updates

Change the assistant's behavior based on context:

```rust
// Start with default prompt
let mut agent = VoiceAgent::new(config);
agent.start(capture_rx, playback_tx, event_handler).await?;

// Later, update system prompt based on user preference
if user_wants_technical_mode {
    agent.set_llm_system_prompt(
        "You are a technical expert. Provide detailed technical explanations."
    ).await;
} else {
    agent.set_llm_system_prompt(
        "You are a friendly assistant. Keep responses simple and conversational."
    ).await;
}
```

### Example: Saving and Restoring Conversations

```rust
use voice_agent::voice_agent::VoiceAgent;
use voice_agent::llm::{LlmHistory, Message};

// Save conversation before shutdown
let history = agent.get_llm_history().await;
let messages = history.get_messages();
// Serialize and save to database/file
save_to_storage(&messages);

// Later, restore conversation
let saved_messages = load_from_storage();
let mut restored_history = LlmHistory::new();
for msg in saved_messages {
    restored_history.add_message(msg);
}
agent.set_llm_history(restored_history).await;
```

### Direct LlmClient Access (Advanced)

If you have direct access to the `LlmClient` instance, you can use its methods directly:

```rust
use voice_agent::llm::{LlmClient, LlmConfig, LlmHistory, Message};

// Direct access to LlmClient methods
let history = llm_client.history().await;
llm_client.set_history(new_history).await;
llm_client.set_system_prompt("New prompt".to_string()).await;
let prompt = llm_client.get_system_prompt().await;
llm_client.clear_history().await;
llm_client.add_message(Message::user("Test")).await;
```

### Available VoiceAgent Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `get_llm_history()` | Get current conversation history | `LlmHistory` |
| `set_llm_history(history)` | Replace entire conversation history | `()` |
| `get_llm_system_prompt()` | Get current system prompt | `String` |
| `set_llm_system_prompt(prompt)` | Update system prompt | `()` |
| `clear_llm_history()` | Clear all messages, reset to default system prompt | `()` |

### LlmHistory Methods

The `LlmHistory` struct provides additional methods for working with history:

| Method | Description | Returns |
|--------|-------------|---------|
| `get_messages()` | Get all messages as a vector | `Vec<Message>` |
| `get_system_prompt()` | Get the system prompt string | `String` |
| `len()` | Get number of messages | `usize` |
| `get(index)` | Get a message by index | `Option<&Message>` |
| `add_message(message)` | Add a message to history | `()` |
| `set_system_prompt(prompt)` | Update system prompt | `()` |
| `clear()` | Clear all messages, reset to default | `()` |

**Indexing**: You can also access messages by index: `history[0]` returns the first message.

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

