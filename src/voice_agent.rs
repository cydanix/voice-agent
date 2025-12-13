use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::StreamExt;
use rust_gradium::{SttConfig, SttEvent, TtsConfig, TtsEvent};
use rust_gradium::wg::WaitGroup;

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::signal::unix::{signal, SignalKind};
use tracing::{info, error, debug, warn};

use crate::llm::{LlmClient, LlmConfig, LlmChunk};
use crate::pcm_capture::{PcmCapture, PcmCaptureMessage};
use crate::pcm_playback::{PcmPlayback, PcmPlaybackMessage};
use crate::stt_handle::SttHandle;
use crate::tts_handle::TtsHandle;
use std::time::Instant;

pub struct VoiceAgent {
    config: Config,
    stt: Option<Arc<SttHandle>>,
    tts: Option<Arc<TtsHandle>>,
    llm: Option<Arc<LlmClient>>,
    capture: Option<PcmCapture>,
    playback: Option<PcmPlayback>,
    wg: WaitGroup,
    event_tx: Option<UnboundedSender<VoiceAgentEvent>>,
}

pub struct Config {
    pub stt_api_key: String,
    pub stt_endpoint: String,
    pub stt_language: String,
    pub tts_api_key: String,
    pub tts_endpoint: String,
    pub tts_voice_id: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub llm_endpoint: String,
    pub llm_system_prompt: String,
}

enum VoiceAgentEvent {
    UserInput(String),
    UserBreak(String),
    TtsError(String),
    SttError(String),
    TtsCloseOrEos,
    SttCloseOrEos,
    Error,
    LlmResponseChunk(String),
    LlmResponseDone(String),
    Shutdown,
}

impl VoiceAgent {
    /// Create a new VoiceAgent instance
    pub fn new(config: Config) -> Self {
        Self {
            config,
            stt: None,
            tts: None,
            llm: None,
            capture: None,
            playback: None,
            wg: WaitGroup::new(),
            event_tx: None,
        }
    }

    /// Start the voice agent - connects to STT/TTS and starts audio processing
    pub async fn start(&mut self) -> anyhow::Result<()> {
        // Create channels
        let (capture_tx, capture_rx) = unbounded_channel::<PcmCaptureMessage>();
        let (playback_tx, playback_rx) = unbounded_channel::<PcmPlaybackMessage>();
        let (event_tx, event_rx) = unbounded_channel::<VoiceAgentEvent>();

        // Create STT handle
        let stt_config = SttConfig::new(self.config.stt_endpoint.clone(),
            self.config.stt_api_key.clone(),
            self.config.stt_language.clone(),
        );
        let stt = Arc::new(SttHandle::new(stt_config));

        // Create TTS handle
        let tts_config = TtsConfig::new(
            self.config.tts_endpoint.clone(),
            self.config.tts_voice_id.clone(),
            self.config.tts_api_key.clone(),
        );
        let tts = Arc::new(TtsHandle::new(tts_config));

        // Create LLM client
        let llm_config = LlmConfig::new(
            self.config.llm_endpoint.clone(),
            self.config.llm_api_key.clone(),
            self.config.llm_model.clone(),
        ).with_system_prompt(&self.config.llm_system_prompt);
        let llm = Arc::new(LlmClient::new(llm_config));
        info!("LLM client created");

        // Connect STT
        if let Err(e) = stt.connect().await {
            error!("failed to connect STT: {e}");
            return Err(anyhow::anyhow!("STT connection failed: {e}"));
        }
        info!("STT connected");

        // Connect TTS
        if let Err(e) = tts.connect().await {
            error!("failed to connect TTS: {e}");
            stt.shutdown().await;
            return Err(anyhow::anyhow!("TTS connection failed: {e}"));
        }
        info!("TTS connected");

        // Create audio capture
        let capture = match PcmCapture::new(capture_tx) {
            Ok(c) => c,
            Err(e) => {
                error!("failed to create PcmCapture: {e}");
                tts.shutdown().await;
                stt.shutdown().await;
                return Err(e);
            }
        };

        // Create audio playback
        let playback = match PcmPlayback::new(playback_rx) {
            Ok(p) => p,
            Err(e) => {
                error!("failed to create PcmPlayback: {e}");
                tts.shutdown().await;
                stt.shutdown().await;
                return Err(e);
            }
        };

        // Start audio streams
        if let Err(e) = capture.start() {
            error!("failed to start capture: {e}");
            tts.shutdown().await;
            stt.shutdown().await;
            return Err(e);
        }

        if let Err(e) = playback.start() {
            error!("failed to start playback: {e}");
            capture.stop();
            tts.shutdown().await;
            stt.shutdown().await;
            return Err(e);
        }

        // Create channel for user input to LLM
        let (llm_input_tx, llm_input_rx) = unbounded_channel::<String>();

        // Spawn background tasks
        self.spawn_stt_ping_task(Arc::clone(&stt));
        self.spawn_tts_ping_task(Arc::clone(&tts));
        self.spawn_capture_task(capture_rx, Arc::clone(&stt));
        self.spawn_stt_event_task(Arc::clone(&stt), event_tx.clone());
        self.spawn_llm_task(Arc::clone(&llm), Arc::clone(&tts), llm_input_rx, event_tx.clone());
        self.spawn_tts_event_task(Arc::clone(&tts), playback_tx.clone(), event_tx.clone());
        self.spawn_main_loop_task(Arc::clone(&stt), Arc::clone(&tts), Arc::clone(&llm), playback_tx, event_rx, llm_input_tx);

        // Store handles
        self.stt = Some(stt);
        self.tts = Some(tts);
        self.llm = Some(llm);
        self.capture = Some(capture);
        self.playback = Some(playback);
        self.event_tx = Some(event_tx);

        info!("voice agent started");
        Ok(())
    }

    /// Run the voice agent until shutdown signal is received
    pub async fn run(&self) -> anyhow::Result<()> {
        info!("running… press Ctrl-C or send SIGTERM to stop");

        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sigint = signal(SignalKind::interrupt())?;

        loop {
            tokio::select! {
                _ = sigterm.recv() => {
                    info!("received SIGTERM, shutting down");
                    break;
                }
                _ = sigint.recv() => {
                    info!("received SIGINT (Ctrl-C), shutting down");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Shutdown the voice agent gracefully
    pub async fn shutdown(&mut self) {
        // Set final shutdown flags first (atomic, immediate signal to event loops)
        if let Some(ref stt) = self.stt {
            stt.set_final_shutdown();
        }
        if let Some(ref tts) = self.tts {
            tts.set_final_shutdown();
        }

        // Stop audio capture (fast, non-blocking)
        if let Some(ref capture) = self.capture {
            info!("stopping audio capture");
            capture.stop();
        }

        if let Some(ref stt) = self.stt {
            info!("shutting down STT");
            stt.shutdown().await;
        }

        if let Some(ref tts) = self.tts {
            info!("shutting down TTS");
            tts.shutdown().await;
        }

        // Close channels to signal tasks to stop (after shutdowns are in progress)
        info!("closing channels");
        // Stop audio playback (after closing playback_tx)
        if let Some(ref playback) = self.playback {
            info!("stopping audio playback");
            playback.stop();
        }

        // Signal shutdown to all tasks
        info!("signalling shutdown to all tasks");
        if let Some(ref event_tx) = self.event_tx {
            if let Err(_) = event_tx.send(VoiceAgentEvent::Shutdown) {
                warn!("event channel receiver already closed, shutdown signal not needed");
            }
        }
        self.event_tx.take();

        // Wait for all tasks to finish
        info!("waiting for all tasks to finish");
        self.wg.wait().await;

        info!("shutdown complete");
    }

    // ========================================================================
    // Private task spawners
    // ========================================================================

    const STT_PING_INTERVAL_MS: u64 = 3000;
    fn spawn_stt_ping_task(&self, stt: Arc<SttHandle>) {
        let wg_guard = self.wg.add();
        tokio::spawn(async move {
            let _wg_guard = wg_guard;
            loop {
                if let Err(e) = stt.sleep(Self::STT_PING_INTERVAL_MS).await {
                    warn!("STT ping sleep error: {e}");
                    break;
                }

                info!("STT sendingping");
                if let Err(e) = stt.send_ping().await {
                    warn!("STT ping error: {e}");
                }
            }
            info!("STT ping task stopped");
        });
    }

    const TTS_PING_INTERVAL_MS: u64 = 3000;
    fn spawn_tts_ping_task(&self, tts: Arc<TtsHandle>) {
        let wg_guard = self.wg.add();
        tokio::spawn(async move {
            let _wg_guard = wg_guard;
            loop {
                if let Err(e) = tts.sleep(Self::TTS_PING_INTERVAL_MS).await {
                    warn!("TTS ping sleep error: {e}");
                    break;
                }

                info!("TTS sending ping");
                if let Err(e) = tts.send_ping().await {
                    warn!("TTS ping error: {e}");
                }
            }
            info!("TTS ping task stopped");
        });
    }

    fn spawn_capture_task(
        &self,
        mut capture_rx: tokio::sync::mpsc::UnboundedReceiver<PcmCaptureMessage>,
        stt: Arc<SttHandle>
    ) {
        let wg_guard = self.wg.add();
        tokio::spawn(async move {
            let _wg_guard = wg_guard;
            let mut audio_chunks_sent = 0u64;
            let mut chunks_sent = 0u64;
            let mut buffer: Vec<u8> = Vec::new();
            let mut critical_error = false;
            loop {
                debug!("capture task loop");
                if critical_error {
                    break;
                }
                // PcmCapture now outputs 24kHz samples directly
                if let Some(msg) = capture_rx.recv().await {
                    let pcm_24k = match msg {
                        PcmCaptureMessage::Chunk(pcm_24k) => pcm_24k,
                    };

                    // Log periodically
                    audio_chunks_sent += 1;
                    if audio_chunks_sent % 100 == 1 {
                        info!("audio capture: chunk #{}, {} samples", audio_chunks_sent, pcm_24k.len());
                    }

                    // Convert to bytes (little-endian i16)
                    let bytes: Vec<u8> = pcm_24k.iter().flat_map(|s| s.to_le_bytes()).collect();
                    buffer.extend(bytes);

                    while buffer.len() >= SttHandle::STT_CHUNK_BYTES {
                        let chunk: Vec<u8> = buffer.drain(..SttHandle::STT_CHUNK_BYTES).collect();
                        let b64 = BASE64.encode(&chunk);

                        chunks_sent += 1;
                        if chunks_sent % 100 == 1 {
                            info!("STT sender: chunk #{}, {} bytes", chunks_sent, chunk.len());
                        }

                        if let Err(e) = stt.process(&b64).await {
                            error!("STT process error: {e}");
                            critical_error = true;
                            break;
                        }
                    }
                } else {
                    info!("capture task stopping");
                    break;
                }
            }
            info!("capture task stopped");
        });
    }

    const VAD_INDEX_TO_CHECK: usize = 2;
    fn spawn_stt_event_task(&self, stt: Arc<SttHandle>, event_tx: UnboundedSender<VoiceAgentEvent>) {
        let wg_guard = self.wg.add();
        tokio::spawn(async move {
            let _wg_guard = wg_guard;
            let mut pending_text = String::new();
            let mut flushed = false;
            let mut flush_ts = Instant::now();
            loop {
                debug!("STT event loop");
                if stt.is_reconnecting() {
                    debug!("STT reconnecting, skipping event loop");
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    if stt.is_final_shutdown() {
                        info!("STT final shutdown, stopping event loop");
                        break;
                    }
                    continue;
                }
                if stt.is_final_shutdown() {
                    info!("STT final shutdown, stopping event loop");
                    break;
                }

                match stt.next_event().await {
                    Ok(Some(event)) => {
                        match event {
                            SttEvent::Step { vad, .. } => {
                                let mut inactivity_prob = 0.0f32;
                                if vad.len() > Self::VAD_INDEX_TO_CHECK {
                                    inactivity_prob = vad[Self::VAD_INDEX_TO_CHECK].inactivity_prob;
                                }
                                info!("inactivity_prob: {inactivity_prob}");

                                if !pending_text.is_empty() && inactivity_prob > 0.6 && !flushed {
                                    info!("Flushing STT by silence");
                                    if let Err(e) = stt.flush_by_silence().await {
                                        warn!("Failed to flush STT by silence: {e}");
                                    } else {
                                        info!("STT flushed by silence");
                                    }
                                    flushed = true;
                                    flush_ts = Instant::now();
                                }

                                if !pending_text.is_empty() && flushed && flush_ts.elapsed().as_millis() > 600 {
                                    let user_input = pending_text.to_string();
                                    pending_text.clear();

                                    info!("User said: '{}'", user_input);

                                    // Send to LLM task
                                    if let Err(e) = event_tx.send(VoiceAgentEvent::UserInput(user_input)) {
                                        warn!("Failed to send to LLM task: {e}");
                                    }

                                    flushed = false;
                                }
                            }
                            SttEvent::Text { text, start } => {
                                let trimmed_text = text.trim();
                                info!("STT text: '{text}' at {start:.2}s");
                                if !trimmed_text.is_empty() {
                                    pending_text.push(' ');
                                    pending_text.push_str(&trimmed_text);

                                    if let Err(e) = event_tx.send(VoiceAgentEvent::UserBreak(trimmed_text.to_string())) {
                                        warn!("Failed to send to UserBreak event: {e}");
                                    }
                                }
                            }
                            SttEvent::EndText { stop } => {
                                debug!("STT end text at {stop:.2}s");
                            }
                            SttEvent::Ready { request_id, model_name, sample_rate, frame_size, delay_in_frames } => {
                                info!("STT ready: request_id={request_id}, model={model_name}, sample_rate={sample_rate}, frame_size={frame_size}, delay_in_frames={delay_in_frames}");
                            }
                            SttEvent::Error { message, code } => {
                                error!("STT error: {message} (code: {code})");

                                stt.set_reconnecting();
                                if let Err(e) = event_tx.send(VoiceAgentEvent::SttError(format!("STT error: {message} (code: {code})"))) {
                                    warn!("Failed to send to SttError event: {e}");
                                }
                            }
                            SttEvent::EndOfStream|SttEvent::Close => {
                                info!("STT end of stream or connection closed, event: {event:?}");

                                stt.set_reconnecting();
                                if let Err(e) = event_tx.send(VoiceAgentEvent::SttCloseOrEos) {
                                    warn!("Failed to send to SttCloseOrEos event: {e}");
                                }
                            }
                            SttEvent::Ping => {
                                info!("STT ping");
                            }
                            SttEvent::Pong => {
                                info!("STT pong");
                            }
                            SttEvent::Frame => {
                                debug!("STT frame");
                            }
                        }
                    }
                    Ok(None) => {
                        error!("STT client not connected");
                        stt.set_reconnecting();
                        if let Err(e) = event_tx.send(VoiceAgentEvent::SttError("STT client not connected".to_string())) {
                            warn!("Failed to send to SttError event: {e}");
                        }
                    }
                    Err(e) => {
                        error!("STT event error: {e}");
                        stt.set_reconnecting();
                        if let Err(e) = event_tx.send(VoiceAgentEvent::SttError(format!("STT event error: {e}"))) {
                            warn!("Failed to send to SttError event: {e}");
                        }
                    }
                }
            }
            info!("STT event loop stopped");
        });
    }

    fn spawn_llm_task(
        &self,
        llm: Arc<LlmClient>,
        _tts: Arc<TtsHandle>,
        mut llm_input_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
        event_tx: UnboundedSender<VoiceAgentEvent>,
    ) {
        let wg_guard = self.wg.add();
        tokio::spawn(async move {
            let _wg_guard = wg_guard;
            loop {
                match llm_input_rx.recv().await {
                    Some(user_input) => {
                        info!("LLM processing: '{}'", user_input);

                        // Reset cancellation and start streaming
                        llm.reset_cancel();
                        match llm.chat_stream(&user_input).await {
                            Ok(mut stream) => {
                                let mut full_response = String::new();
                                let mut sentence_buffer = String::new();

                                while let Some(chunk) = stream.next().await {
                                    match chunk {
                                        LlmChunk::Delta(text) => {
                                            debug!("LLM delta: '{}'", text);
                                            full_response.push_str(&text);
                                            sentence_buffer.push_str(&text);

                                            // Send complete sentences to TTS for faster response
                                            while let Some(pos) = sentence_buffer.find(['.', '!', '?', '\n']) {
                                                let to_send = &sentence_buffer[..=pos];
                                                if !to_send.trim().is_empty() {
                                                    info!("LLM sending to TTS: '{}'", to_send.trim());
                                                    if let Err(e) = event_tx.send(VoiceAgentEvent::LlmResponseChunk(to_send.trim().to_string())) {
                                                        warn!("Failed to send to LlmResponseChunk event: {e}");
                                                    }
                                                }
                                                sentence_buffer = sentence_buffer[pos + 1..].to_string();
                                            }
                                        }
                                        LlmChunk::Done => {
                                            info!("LLM response complete");
                                            // Send any remaining text
                                            let last_text = sentence_buffer.trim().to_string();
                                            if let Err(e) = event_tx.send(VoiceAgentEvent::LlmResponseDone(last_text)) {
                                                warn!("Failed to send to LlmResponseDone event: {e}");
                                            }
                                            info!("LLM response: '{}'", full_response);
                                            break;
                                        }
                                        LlmChunk::Cancelled => {
                                            info!("LLM stream cancelled");
                                            break;
                                        }
                                    }
                                }

                                // Add assistant response to history
                                llm.add_assistant_response(&full_response).await;
                            }
                            Err(e) => {
                                error!("LLM chat error: {e}");
                                if let Err(e) = event_tx.send(VoiceAgentEvent::Error) {
                                    warn!("Failed to send to Error event: {e}");
                                }
                            }
                        }
                    }
                    None => {
                        info!("LLM task stopping (channel closed)");
                        break;
                    }
                }
            }
            info!("LLM event loop stopped");
        });
    }

    fn spawn_tts_event_task(&self, tts: Arc<TtsHandle>, playback_tx: UnboundedSender<PcmPlaybackMessage>, event_tx: UnboundedSender<VoiceAgentEvent>) {
        let wg_guard = self.wg.add();
        tokio::spawn(async move {
            let _wg_guard = wg_guard;
            loop {
                debug!("TTS event loop");
                if tts.is_reconnecting() {
                    debug!("TTS reconnecting, skipping event loop");
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    if tts.is_final_shutdown() {
                        info!("TTS final shutdown, stopping event loop");
                        break;
                    }
                    continue;
                }
                if tts.is_final_shutdown() {
                    info!("TTS final shutdown, stopping event loop");
                    break;
                }

                match tts.next_event().await {
                    Ok(Some(event)) => {
                        match event {
                            TtsEvent::Audio { audio } => {
                                match BASE64.decode(&audio) {
                                    Ok(bytes) => {
                                        let samples: Vec<i16> = bytes
                                            .chunks_exact(2)
                                            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                                            .collect();

                                        debug!("TTS audio: {} samples", samples.len());
                                        if let Err(e) = playback_tx.send(PcmPlaybackMessage::Play(samples)) {
                                            warn!("playback send error: {e}");
                                        }
                                    }
                                    Err(e) => {
                                        warn!("TTS audio decode error: {e}");
                                    }
                                }
                            }
                            TtsEvent::Ready { request_id } => {
                                info!("TTS ready: request_id={request_id}");
                            }
                            TtsEvent::TextEcho { text } => {
                                debug!("TTS text echo: '{text}'");
                            }
                            TtsEvent::Error { message, code } => {
                                error!("TTS error: {message} (code: {code})");

                                tts.set_reconnecting();
                                if let Err(e) = event_tx.send(VoiceAgentEvent::TtsError(message)) {
                                    warn!("Failed to send to TtsError event: {e}");
                                }
                            }
                            TtsEvent::EndOfStream|TtsEvent::Close => {
                                info!("TTS end of stream or connection closed, event: {event:?}");

                                tts.set_reconnecting();
                                if let Err(e) = event_tx.send(VoiceAgentEvent::TtsCloseOrEos) {
                                    warn!("Failed to send to TtsCloseOrEos event: {e}");
                                }
                            }
                            TtsEvent::Ping => {
                                info!("TTS ping");
                            }
                            TtsEvent::Pong => {
                                info!("TTS pong");
                            }
                            TtsEvent::Frame => {
                                debug!("TTS frame");
                            }
                        }
                    }
                    Ok(None) => {
                        error!("TTS client not connected");
                        tts.set_reconnecting();
                        if let Err(e) = event_tx.send(VoiceAgentEvent::TtsError("TTS client not connected".to_string())) {
                            warn!("Failed to send to TtsError event: {e}");
                        }
                    }
                    Err(e) => {
                        error!("TTS event error: {e}");
                        tts.set_reconnecting();
                        if let Err(e) = event_tx.send(VoiceAgentEvent::TtsError(e.to_string())) {
                            warn!("Failed to send to TtsError event: {e}");
                        }
                    }
                }
            }
            info!("TTS event loop stopped");
        });
    }


    fn spawn_main_loop_task(
        &self,
        stt: Arc<SttHandle>,
        tts: Arc<TtsHandle>,
        llm: Arc<LlmClient>,
        playback_tx: UnboundedSender<PcmPlaybackMessage>,
        mut event_rx: tokio::sync::mpsc::UnboundedReceiver<VoiceAgentEvent>,
        llm_input_tx: UnboundedSender<String>,
    ) {
        let wg_guard = self.wg.add();
        tokio::spawn(async move {
            let _wg_guard = wg_guard;
            loop {
                match event_rx.recv().await {
                    Some(event) => {
                        match event {
                            VoiceAgentEvent::UserInput(input) => {
                                info!("User input: '{}'", input);
                                if let Err(e) = llm_input_tx.send(input) {
                                    error!("Failed to send to LLM task: {e}");
                                    break;
                                }
                            }
                            VoiceAgentEvent::UserBreak(text) => {
                                info!("User break: '{}'", text);
                                llm.cancel();
                                if let Err(e) = tts.cancel().await {
                                    error!("Failed to cancel TTS: {e}");
                                    break;
                                }
                                if let Err(e) = playback_tx.send(PcmPlaybackMessage::Reset) {
                                    error!("Failed to send to playback task: {e}");
                                    break;
                                }
                                info!("User break complete");
                            }
                            VoiceAgentEvent::TtsCloseOrEos => {
                                info!("TTS close or eos");
                                if let Err(e) = tts.reconnect().await {
                                    error!("TTS reconnect error: {e}");
                                    break;
                                }
                            }
                            VoiceAgentEvent::TtsError(message) => {
                                info!("TTS error: {message}");
                                if let Err(e) = tts.reconnect().await {
                                    error!("TTS reconnect error: {e}");
                                    break;
                                }
                            }
                            VoiceAgentEvent::SttCloseOrEos => {
                                info!("STT close or eos");
                                if let Err(e) = stt.reconnect().await {
                                    error!("STT reconnect error: {e}");
                                    break;
                                }
                            }
                            VoiceAgentEvent::SttError(message) => {
                                info!("STT error: {message}");
                                if let Err(e) = stt.reconnect().await {
                                    error!("STT reconnect error: {e}");
                                    break;
                                }
                            }
                            VoiceAgentEvent::Error => {
                                error!("Received Error event");
                                break;
                            }
                            VoiceAgentEvent::LlmResponseChunk(text) => {
                                info!("LLM response chunk: '{}'", text);
                                if let Err(e) = tts.process(&text).await {
                                    error!("TTS process error: {e}");
                                    break;
                                }
                            }
                            VoiceAgentEvent::LlmResponseDone(text) => {
                                info!("LLM response done: '{}'", text);
                                if !text.is_empty() {
                                    if let Err(e) = tts.process(&text).await {
                                        error!("TTS process error: {e}");
                                        break;
                                    }
                                }
                                if let Err(e) = tts.send_eos().await {
                                    error!("TTS send eos error: {e}");
                                    break;
                                }
                            }
                            VoiceAgentEvent::Shutdown => {
                                info!("Shutting down voice agent");
                                break;
                            }
                        }
                    }
                    None => {
                        info!("Main loop task stopping (channel closed)");
                        break;
                    }
                }
            }
            info!("Main loop task stopped");
        });
    }
}
