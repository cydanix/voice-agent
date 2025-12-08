use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::StreamExt;
use rust_gradium::{SttConfig, SttEvent, TtsConfig, TtsEvent};
use rust_gradium::wg::WaitGroup;

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::Mutex;
use tracing::{info, error, debug, warn};

use crate::llm::{LlmClient, LlmConfig, LlmChunk};
use crate::pcm_capture::PcmCapture;
use crate::pcm_playback::{PcmPlayback, PcmPlaybackController};
use crate::stt_handle::SttHandle;
use crate::tts_handle::TtsHandle;
use crate::pause_detector::PauseDetector;

pub struct VoiceAgent {
    config: Config,
    stt: Option<Arc<SttHandle>>,
    tts: Option<Arc<TtsHandle>>,
    llm: Option<Arc<LlmClient>>,
    capture: Option<PcmCapture>,
    playback: Option<PcmPlayback>,
    wg: WaitGroup,
    pause_detector: Arc<Mutex<PauseDetector>>,
}

pub struct Config {
    pub tts_api_key: String,
    pub tts_endpoint: String,
    pub stt_api_key: String,
    pub stt_endpoint: String,
    pub voice_id: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub llm_endpoint: String,
    pub system_prompt: String,
}

enum VoiceAgentEvent {
    UserInput(String),
    UserBreak(String),
    TtsCloseOrEos,
    SttCloseOrEos,
    Error,
    LlmResponseChunk(String),
    LlmResponseDone(String),
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
            pause_detector: Arc::new(Mutex::new(PauseDetector::new())),
        }
    }

    /// Start the voice agent - connects to STT/TTS and starts audio processing
    pub async fn start(&mut self) -> anyhow::Result<()> {
        // Create channels
        let (capture_tx, capture_rx) = unbounded_channel::<Vec<i16>>();
        let (playback_tx, playback_rx) = unbounded_channel::<Vec<i16>>();
        let (stt_bytes_tx, stt_bytes_rx) = unbounded_channel::<Vec<u8>>();
        let (event_tx, event_rx) = unbounded_channel::<VoiceAgentEvent>();

        // Create STT handle
        let stt_config = SttConfig::new(self.config.stt_endpoint.clone(), self.config.stt_api_key.clone());
        let stt = Arc::new(SttHandle::new(stt_config));

        // Create TTS handle
        let tts_config = TtsConfig::new(
            self.config.tts_endpoint.clone(),
            self.config.voice_id.clone(),
            self.config.tts_api_key.clone(),
        );
        let tts = Arc::new(TtsHandle::new(tts_config));

        // Create LLM client
        let llm_config = LlmConfig::new(
            self.config.llm_endpoint.clone(),
            self.config.llm_api_key.clone(),
            self.config.llm_model.clone(),
        ).with_system_prompt(&self.config.system_prompt);
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
        let playback_controller = playback.controller();

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
        self.spawn_capture_task(stt_bytes_tx, capture_rx);
        self.spawn_stt_sender_task(Arc::clone(&stt), stt_bytes_rx);
        self.spawn_stt_event_task(Arc::clone(&stt), event_tx.clone());
        self.spawn_llm_task(Arc::clone(&llm), Arc::clone(&tts), llm_input_rx, event_tx.clone());
        self.spawn_tts_event_task(Arc::clone(&tts), playback_tx, event_tx.clone());
        self.spawn_main_loop_task(Arc::clone(&stt), Arc::clone(&tts), Arc::clone(&llm), playback_controller, event_rx, llm_input_tx.clone());

        // Store handles
        self.stt = Some(stt);
        self.tts = Some(tts);
        self.llm = Some(llm);
        self.capture = Some(capture);
        self.playback = Some(playback);

        info!("voice agent started");
        Ok(())
    }

    /// Run the voice agent until shutdown signal is received
    pub async fn run(&self) -> anyhow::Result<()> {
        info!("running… press Ctrl-C or send SIGTERM to stop");

        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sigint = signal(SignalKind::interrupt())?;

        tokio::select! {
            _ = sigterm.recv() => {
                info!("received SIGTERM, shutting down");
            }
            _ = sigint.recv() => {
                info!("received SIGINT (Ctrl-C), shutting down");
            }
        }

        Ok(())
    }

    /// Shutdown the voice agent gracefully
    pub async fn shutdown(&mut self) {
        // Stop audio capture
        if let Some(ref capture) = self.capture {
            info!("stopping audio capture");
            capture.stop();
        }

        // Shutdown STT
        if let Some(ref stt) = self.stt {
            info!("shutting down STT");
            stt.set_final_shutdown();
            stt.shutdown().await;
        }

        // Shutdown TTS
        if let Some(ref tts) = self.tts {
            info!("shutting down TTS");
            tts.set_final_shutdown();
            tts.shutdown().await;
        }

        // Stop audio playback
        if let Some(ref playback) = self.playback {
            info!("stopping audio playback");
            playback.stop();
        }

        // Wait for all tasks to finish
        info!("waiting for all tasks to finish");
        self.wg.wait().await;

        info!("shutdown complete");
    }

    // ========================================================================
    // Private task spawners
    // ========================================================================

    fn spawn_stt_ping_task(&self, stt: Arc<SttHandle>) {
        let wg_guard = self.wg.add();
        tokio::spawn(async move {
            let _wg_guard = wg_guard;
            let mut interval = tokio::time::interval(Duration::from_secs(3));
            loop {
                interval.tick().await;
                info!("sending STT ping");
                if let Err(e) = stt.send_ping().await {
                    warn!("STT ping error: {e}");
                    if stt.is_final_shutdown() {
                        info!("STT final shutdown, stopping ping task");
                        break;
                    }
                }
            }
        });
    }

    fn spawn_tts_ping_task(&self, tts: Arc<TtsHandle>) {
        let wg_guard = self.wg.add();
        tokio::spawn(async move {
            let _wg_guard = wg_guard;
            let mut interval = tokio::time::interval(Duration::from_secs(3));
            loop {
                interval.tick().await;
                info!("sending TTS ping");
                if let Err(e) = tts.send_ping().await {
                    warn!("TTS ping error: {e}");
                    if tts.is_final_shutdown() {
                        info!("TTS final shutdown, stopping ping task");
                        break;
                    }
                }
            }
        });
    }

    fn spawn_capture_task(
        &self,
        stt_bytes_tx: UnboundedSender<Vec<u8>>,
        mut capture_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<i16>>,
    ) {
        let wg_guard = self.wg.add();
        tokio::spawn(async move {
            let _wg_guard = wg_guard;
            let mut audio_chunks_sent = 0u64;
            loop {
                debug!("capture task loop");
                // PcmCapture now outputs 24kHz samples directly
                if let Some(pcm_24k) = capture_rx.recv().await {
                    // Log periodically
                    audio_chunks_sent += 1;
                    if audio_chunks_sent % 100 == 1 {
                        info!("audio capture: chunk #{}, {} samples", audio_chunks_sent, pcm_24k.len());
                    }

                    // Convert to bytes (little-endian i16)
                    let bytes: Vec<u8> = pcm_24k.iter().flat_map(|s| s.to_le_bytes()).collect();

                    // Send to STT bytes queue
                    if let Err(e) = stt_bytes_tx.send(bytes) {
                        warn!("STT bytes queue send error: {e}");
                        break;
                    }
                } else {
                    info!("capture task stopping");
                    break;
                }
            }
        });
    }

    /// 80ms of 24kHz mono PCM = 1920 samples = 3840 bytes
    const STT_CHUNK_BYTES: usize = 3840;

    fn spawn_stt_sender_task(
        &self,
        stt: Arc<SttHandle>,
        mut stt_bytes_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    ) {
        let wg_guard = self.wg.add();
        tokio::spawn(async move {
            let _wg_guard = wg_guard;
            let mut buffer: Vec<u8> = Vec::new();
            let mut chunks_sent = 0u64;

            loop {
                if let Some(bytes) = stt_bytes_rx.recv().await {
                    buffer.extend(bytes);

                    // Send complete 80ms chunks to STT
                    while buffer.len() >= Self::STT_CHUNK_BYTES {
                        let chunk: Vec<u8> = buffer.drain(..Self::STT_CHUNK_BYTES).collect();
                        let b64 = BASE64.encode(&chunk);

                        chunks_sent += 1;
                        if chunks_sent % 100 == 1 {
                            info!("STT sender: chunk #{}, {} bytes", chunks_sent, chunk.len());
                        }

                        if let Err(e) = stt.process(&b64).await {
                            error!("STT process error: {e}");
                            break;
                        }
                    }
                } else {
                    // Channel closed, flush remaining buffer
                    if !buffer.is_empty() {
                        debug!("STT sender: flushing {} remaining bytes", buffer.len());
                        let b64 = BASE64.encode(&buffer);
                        if let Err(e) = stt.process(&b64).await {
                            error!("STT process error on flush: {e}");
                            break;
                        }
                    }
                    info!("STT sender task stopping");
                    break;
                }
            }
        });
    }

    const VAD_INDEX_TO_CHECK: usize = 2;
    fn spawn_stt_event_task(&self, stt: Arc<SttHandle>, event_tx: UnboundedSender<VoiceAgentEvent>) {
        let pause_detector = self.pause_detector.clone();
        let wg_guard = self.wg.add();
        tokio::spawn(async move {
            let _wg_guard = wg_guard;
            let mut pending_text = String::new();
            loop {
                debug!("STT event loop");
                if stt.is_reconnecting() {
                    debug!("STT reconnecting, skipping event loop");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    if stt.is_final_shutdown() {
                        info!("STT final shutdown, stopping event loop");
                        break;
                    }
                    continue;
                }

                match stt.next_event().await {
                    Ok(Some(event)) => {
                        match event {
                            SttEvent::Step { vad, .. } => {
                                let mut inactivity_prob = 0.0f32;
                                if vad.len() > Self::VAD_INDEX_TO_CHECK {
                                    inactivity_prob = vad[Self::VAD_INDEX_TO_CHECK].inactivity_prob;
                                }

                                let mut pd = pause_detector.lock().await;
                                pd.add_inactivity_prob(inactivity_prob);

                                if pd.is_paused() && !pending_text.trim().is_empty() {
                                    let user_input = pending_text.trim().to_string();
                                    pending_text.clear();

                                    info!("User said: '{}'", user_input);

                                    // Send to LLM task
                                    if let Err(e) = event_tx.send(VoiceAgentEvent::UserInput(user_input)) {
                                        warn!("Failed to send to LLM task: {e}");
                                    }

                                    pd.reset();
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
                                pause_detector.lock().await.add_text(&trimmed_text);
                            }
                            SttEvent::EndText { stop } => {
                                debug!("STT end text at {stop:.2}s");
                            }
                            SttEvent::Ready { request_id, model_name, sample_rate } => {
                                info!("STT ready: request_id={request_id}, model={model_name}, sample_rate={sample_rate}");
                            }
                            SttEvent::Error { message, code } => {
                                error!("STT error: {message} (code: {code})");
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
                        break;
                    }
                    Err(e) => {
                        error!("STT event error: {e}");
                        break;
                    }
                }
            }
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
        });
    }

    fn spawn_tts_event_task(&self, tts: Arc<TtsHandle>, playback_tx: UnboundedSender<Vec<i16>>, event_tx: UnboundedSender<VoiceAgentEvent>) {
        let wg_guard = self.wg.add();
        tokio::spawn(async move {
            let _wg_guard = wg_guard;
            loop {
                debug!("TTS event loop");
                if tts.is_reconnecting() {
                    debug!("TTS reconnecting, skipping event loop");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    if tts.is_final_shutdown() {
                        info!("TTS final shutdown, stopping event loop");
                        break;
                    }
                    continue;
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
                                        if let Err(e) = playback_tx.send(samples) {
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
                                if let Err(e) = event_tx.send(VoiceAgentEvent::Error) {
                                    warn!("Failed to send to Error event: {e}");
                                }
                                break;
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
                        if let Err(e) = event_tx.send(VoiceAgentEvent::Error) {
                            warn!("Failed to send to Error event: {e}");
                        }
                        break;
                    }
                    Err(e) => {
                        error!("TTS event error: {e}");
                        if let Err(e) = event_tx.send(VoiceAgentEvent::Error) {
                            warn!("Failed to send to Error event: {e}");
                        }
                        break;
                    }
                }
            }
        });
    }


    fn spawn_main_loop_task(
        &self,
        stt: Arc<SttHandle>,
        tts: Arc<TtsHandle>,
        llm: Arc<LlmClient>,
        playback_controller: PcmPlaybackController,
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
                                if let Err(e) = playback_controller.restart() {
                                    error!("Failed to start playback: {e}");
                                    break;
                                }
                                info!("User break complete");
                            }
                            VoiceAgentEvent::TtsCloseOrEos => {
                                info!("TTS close or EOS");
                                if tts.is_final_shutdown() {
                                    info!("TTS final shutdown");
                                    break;
                                }
                                if let Err(e) = tts.reconnect().await {
                                    error!("TTS reconnect error: {e}");
                                    break;
                                }
                                if tts.is_final_shutdown() {
                                    info!("TTS final shutdown after reconnect");
                                    break;
                                }
                            }
                            VoiceAgentEvent::SttCloseOrEos => {
                                info!("STT close or eos");
                                if stt.is_final_shutdown() {
                                    info!("STT final shutdown");
                                    break;
                                }
                                if let Err(e) = stt.reconnect().await {
                                    error!("STT reconnect error: {e}");
                                    break;
                                }
                                if stt.is_final_shutdown() {
                                    info!("STT final shutdown after reconnect");
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
                        }
                    }
                    None => {
                        info!("Main loop task stopping (channel closed)");
                        break;
                    }
                }
            }
        });
    }
}
