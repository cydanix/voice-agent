use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::StreamExt;
use rust_gradium::{SttConfig, SttEvent, TtsConfig, TtsEvent};
use rust_gradium::wg::WaitGroup;

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tracing::{info, error, debug, warn};
use std::sync::atomic::{AtomicBool, Ordering};
use async_trait::async_trait;

use crate::llm::{LlmClient, LlmConfig, LlmChunk, LlmHistory};
use crate::messages::{AudioCaptureMessage, AudioPlaybackMessage};
use crate::stt_handle::SttHandle;
use crate::tts_handle::TtsHandle;
use std::time::Instant;

enum VoiceAgentEvent {
    UserInput(String),
    UserBreak(String),
    TtsSpeech(String),
    TtsError(String),
    SttError(String),
    TtsCloseOrEos,
    SttCloseOrEos,
    LlmError(String),
    Error(String),
    LlmResponseChunk(String),
    LlmResponseDone(String),
    Shutdown,
}

#[async_trait]
pub trait VoiceAgentEventHandler: Send + Sync {
    async fn on_user_input(&self, input: String) -> String;
    async fn on_user_break(&self, text: String);
    async fn on_tts_speech(&self, text: String);
    async fn on_tts_error(&self, error: String);
    async fn on_stt_error(&self, error: String);
    async fn on_tts_close_or_eos(&self);
    async fn on_stt_close_or_eos(&self);
    async fn on_error(&self, error_message: String);
    async fn on_llm_error(&self, error_message: String);
    async fn on_llm_response_chunk(&self, text: String);
    async fn on_llm_response_done(&self, text: String);
    async fn on_shutdown(&self);
}

/// No-op event handler that does nothing
pub struct VoiceAgentNoOpEventHandler;

#[async_trait]
impl VoiceAgentEventHandler for VoiceAgentNoOpEventHandler {
    async fn on_user_input(&self, input: String) -> String {
        input
    }

    async fn on_user_break(&self, _text: String) {}

    async fn on_tts_speech(&self, _text: String) {}

    async fn on_tts_error(&self, _error: String) {}

    async fn on_stt_error(&self, _error: String) {}

    async fn on_tts_close_or_eos(&self) {}

    async fn on_stt_close_or_eos(&self) {}

    async fn on_error(&self, _error_message: String) {}

    async fn on_llm_error(&self, _error_message: String) {}

    async fn on_llm_response_chunk(&self, _text: String) {}

    async fn on_llm_response_done(&self, _text: String) {}

    async fn on_shutdown(&self) {}
}

pub struct VoiceAgent {
    config: Config,
    stt: Option<Arc<SttHandle>>,
    tts: Option<Arc<TtsHandle>>,
    llm: Option<Arc<LlmClient>>,
    wg: WaitGroup,
    event_tx: Option<UnboundedSender<VoiceAgentEvent>>,
    stopping: AtomicBool,
    event_handler: Option<Arc<dyn VoiceAgentEventHandler>>,
}

#[derive(Clone)]
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

impl VoiceAgent {
    /// Create a new VoiceAgent instance
    pub fn new(config: Config) -> Self {
        Self {
            config,
            stt: None,
            tts: None,
            llm: None,
            wg: WaitGroup::new(),
            event_tx: None,
            stopping: AtomicBool::new(false),
            event_handler: None,
        }
    }

    /// Start the voice agent - connects to STT/TTS and starts audio processing
    pub async fn start(&mut self, capture_rx: UnboundedReceiver<AudioCaptureMessage>,
        playback_tx: UnboundedSender<AudioPlaybackMessage>,
        event_handler: Arc<dyn VoiceAgentEventHandler>) -> anyhow::Result<()> {
        // Create channels
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

        // Create channel for user input to LLM
        let (llm_input_tx, llm_input_rx) = unbounded_channel::<String>();

        // Spawn background tasks
        self.spawn_stt_ping_task(Arc::clone(&stt));
        self.spawn_tts_ping_task(Arc::clone(&tts));
        self.spawn_capture_task(capture_rx, Arc::clone(&stt));
        self.spawn_stt_event_task(Arc::clone(&stt), event_tx.clone());
        self.spawn_llm_task(Arc::clone(&llm), Arc::clone(&tts), llm_input_rx, event_tx.clone());
        self.spawn_tts_event_task(Arc::clone(&tts), playback_tx.clone(), event_tx.clone());
        self.spawn_main_loop_task(Arc::clone(&stt), Arc::clone(&tts), Arc::clone(&llm), playback_tx, event_rx, llm_input_tx, event_handler.clone());

        // Store handles
        self.stt = Some(stt);
        self.tts = Some(tts);
        self.llm = Some(llm);
        self.event_tx = Some(event_tx);
        self.event_handler = Some(event_handler);

        info!("voice agent started");
        Ok(())
    }

    pub fn set_stopping(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Run the voice agent until shutdown signal is received
    pub async fn run(&self) -> anyhow::Result<()> {
        info!("running…");

        loop {
            if self.is_stopping() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        info!("stopping…");
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

        if let Some(ref stt) = self.stt {
            info!("shutting down STT");
            stt.shutdown().await;
        }

        if let Some(ref tts) = self.tts {
            info!("shutting down TTS");
            tts.shutdown().await;
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

    pub fn inject_tts_speech(&self, text: String) {
        if let Some(ref event_tx) = self.event_tx {
            if let Err(e) = event_tx.send(VoiceAgentEvent::TtsSpeech(text)) {
                warn!("Failed to send TTS text: {e}");
            }
        }
    }

    pub fn inject_error(&self, error_message: String) {
        if let Some(ref event_tx) = self.event_tx {
            if let Err(e) = event_tx.send(VoiceAgentEvent::Error(error_message)) {
                warn!("Failed to send error message: {e}");
            }
        }
    }

    pub async fn get_llm_history(&self) -> LlmHistory {
        if let Some(ref llm) = self.llm {
            llm.history().await
        } else {
            warn!("LLM not connected, returning empty history");
            LlmHistory::new()
        }.clone()
    }

    pub async fn set_llm_history(&self, history: LlmHistory) {
        if let Some(ref llm) = self.llm {
            llm.set_history(history).await;
        } else {
            warn!("LLM not connected, skipping history set");
        }
    }

    pub async fn clear_llm_history(&self) {
        if let Some(ref llm) = self.llm {
            llm.clear_history().await;
        } else {
            warn!("LLM not connected, skipping history clear");
        }
    }

    pub async fn set_llm_system_prompt(&self, prompt: String) {
        if let Some(ref llm) = self.llm {
            llm.set_system_prompt(prompt).await;
        } else {
            warn!("LLM not connected, skipping system prompt set");
        }
    }

    pub async fn get_llm_system_prompt(&self) -> String {
        if let Some(ref llm) = self.llm {
            llm.get_system_prompt().await
        } else {
            warn!("LLM not connected, returning empty system prompt");
            String::new()
        }
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

                info!("STT sending ping");
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
        mut capture_rx: tokio::sync::mpsc::UnboundedReceiver<AudioCaptureMessage>,
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
                        AudioCaptureMessage::Chunk(pcm_24k) => pcm_24k,
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

                                if inactivity_prob <= 0.95 {
                                    info!("STT inactivity_prob: {inactivity_prob}");
                                } else {
                                    debug!("STT inactivity_prob: {inactivity_prob}");
                                }

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
                                            while let Some(pos) = sentence_buffer.find([' ', '\t', '.', '!', '?', '\n']) {
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
                                if let Err(e) = event_tx.send(VoiceAgentEvent::LlmError(format!("LLM chat error: {e}"))) {
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

    fn spawn_tts_event_task(&self, tts: Arc<TtsHandle>, playback_tx: UnboundedSender<AudioPlaybackMessage>, event_tx: UnboundedSender<VoiceAgentEvent>) {
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
                                if tts.is_discarding() {
                                    debug!("TTS discard audio event");
                                    continue;
                                }
                                match BASE64.decode(&audio) {
                                    Ok(bytes) => {
                                        let samples: Vec<i16> = bytes
                                            .chunks_exact(2)
                                            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                                            .collect();

                                        debug!("TTS audio: {} samples", samples.len());
                                        if let Err(e) = playback_tx.send(AudioPlaybackMessage::Play(samples)) {
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
        playback_tx: UnboundedSender<AudioPlaybackMessage>,
        mut event_rx: tokio::sync::mpsc::UnboundedReceiver<VoiceAgentEvent>,
        llm_input_tx: UnboundedSender<String>,
        event_handler: Arc<dyn VoiceAgentEventHandler>,
    ) {
        let wg_guard = self.wg.add();
        tokio::spawn(async move {
            let _wg_guard = wg_guard;
            let mut is_new_llm_response = false;
            loop {
                match event_rx.recv().await {
                    Some(event) => {
                        match event {
                            VoiceAgentEvent::UserInput(input) => {
                                info!("User input: '{}'", input);
                                // Mark that a new LLM response is starting
                                is_new_llm_response = true;

                                let result_input = event_handler.on_user_input(input).await;
                                if let Err(e) = llm_input_tx.send(result_input) {
                                    error!("Failed to send to LLM task: {e}");
                                    break;
                                }
                            }
                            VoiceAgentEvent::UserBreak(text) => {
                                info!("User break: '{}'", text);

                                event_handler.on_user_break(text).await;

                                tts.set_discarding();
                                llm.cancel();
                                if let Err(e) = tts.cancel().await {
                                    error!("Failed to cancel TTS: {e}");
                                    break;
                                }
                                if let Err(e) = playback_tx.send(AudioPlaybackMessage::Reset) {
                                    error!("Failed to send to playback task: {e}");
                                    break;
                                }
                                is_new_llm_response = false;
                                info!("User break complete");
                            }
                            VoiceAgentEvent::TtsCloseOrEos => {
                                info!("TTS close or eos");
                                event_handler.on_tts_close_or_eos().await;
                                if let Err(e) = tts.reconnect().await {
                                    error!("TTS reconnect error: {e}");
                                    break;
                                }
                            }
                            VoiceAgentEvent::TtsError(message) => {
                                info!("TTS error: {message}");
                                event_handler.on_tts_error(message).await;
                                if let Err(e) = tts.reconnect().await {
                                    error!("TTS reconnect error: {e}");
                                    break;
                                }
                            }
                            VoiceAgentEvent::SttCloseOrEos => {
                                info!("STT close or eos");
                                event_handler.on_stt_close_or_eos().await;
                                if let Err(e) = stt.reconnect().await {
                                    error!("STT reconnect error: {e}");
                                    break;
                                }
                            }
                            VoiceAgentEvent::SttError(message) => {
                                info!("STT error: {message}");
                                event_handler.on_stt_error(message).await;
                                if let Err(e) = stt.reconnect().await {
                                    error!("STT reconnect error: {e}");
                                    break;
                                }
                            }
                            VoiceAgentEvent::Error(error_message) => {
                                error!("Received Error event: {error_message}");
                                event_handler.on_error(error_message).await;
                                break;
                            }
                            VoiceAgentEvent::LlmResponseChunk(text) => {
                                info!("LLM response chunk: '{}'", text);
                                event_handler.on_llm_response_chunk(text.clone()).await;
                                // Reset playback before first chunk of new LLM response
                                if is_new_llm_response {
                                    info!("Resetting playback for new LLM response");
                                    if let Err(e) = playback_tx.send(AudioPlaybackMessage::Reset) {
                                        error!("Failed to send reset to playback task: {e}");
                                        break;
                                    }
                                    // Small delay to ensure reset is processed
                                    tokio::time::sleep(Duration::from_millis(10)).await;
                                    is_new_llm_response = false;
                                }
                                if !text.is_empty() {
                                    event_handler.on_tts_speech(text.clone()).await;
                                    if let Err(e) = tts.process(&text).await {
                                        error!("TTS process error: {e}");
                                        break;
                                    }
                                }
                            }
                            VoiceAgentEvent::LlmResponseDone(text) => {
                                info!("LLM response done: '{}'", text);
                                event_handler.on_llm_response_done(text.clone()).await;
                                // Reset playback before first chunk if this is the first (and only) chunk
                                if is_new_llm_response {
                                    info!("Resetting playback for new LLM response (done)");
                                    if let Err(e) = playback_tx.send(AudioPlaybackMessage::Reset) {
                                        error!("Failed to send reset to playback task: {e}");
                                        break;
                                    }
                                    // Small delay to ensure reset is processed
                                    tokio::time::sleep(Duration::from_millis(10)).await;
                                    is_new_llm_response = false;
                                }
                                if !text.is_empty() {
                                    event_handler.on_tts_speech(text.clone()).await;
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
                            VoiceAgentEvent::LlmError(error_message) => {
                                warn!("Received LlmError event: {error_message}");
                                event_handler.on_llm_error(error_message).await;
                                if is_new_llm_response {
                                    info!("Resetting playback for new LLM response (done)");
                                    if let Err(e) = playback_tx.send(AudioPlaybackMessage::Reset) {
                                        error!("Failed to send reset to playback task: {e}");
                                        break;
                                    }
                                    // Small delay to ensure reset is processed
                                    tokio::time::sleep(Duration::from_millis(10)).await;
                                    is_new_llm_response = false;
                                }
                                if let Err(e) = tts.send_eos().await {
                                    error!("TTS send eos error: {e}");
                                    break;
                                }
                            }
                            VoiceAgentEvent::TtsSpeech(text) => {
                                info!("TTS speech: '{}'", text);
                                event_handler.on_tts_speech(text.clone()).await;
                                if let Err(e) = tts.process(&text).await {
                                    error!("TTS process error: {e}");
                                    break;
                                }
                                if let Err(e) = tts.send_eos().await {
                                    error!("TTS send eos error: {e}");
                                    break;
                                }
                            }
                            VoiceAgentEvent::Shutdown => {
                                info!("Shutting down voice agent");
                                event_handler.on_shutdown().await;
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
