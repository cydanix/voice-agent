use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rust_gradium::{
    downsample_48_to_24, SttConfig, SttEvent, TtsConfig, TtsEvent,
    STT_ENDPOINT, TTS_ENDPOINT, DEFAULT_VOICE_ID,
};
use rust_gradium::wg::WaitGroup;

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::signal::unix::{signal, SignalKind};
use tracing::{info, error, debug, warn};

use crate::pcm_capture::PcmCapture;
use crate::pcm_playback::PcmPlayback;
use crate::stt_handle::SttHandle;
use crate::tts_handle::TtsHandle;

pub struct VoiceAgent {
    api_key: String,
    stt: Option<Arc<SttHandle>>,
    tts: Option<Arc<TtsHandle>>,
    capture: Option<PcmCapture>,
    playback: Option<PcmPlayback>,
    wg: WaitGroup,
}

impl VoiceAgent {
    /// Create a new VoiceAgent instance
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            stt: None,
            tts: None,
            capture: None,
            playback: None,
            wg: WaitGroup::new(),
        }
    }

    /// Start the voice agent - connects to STT/TTS and starts audio processing
    pub async fn start(&mut self) -> anyhow::Result<()> {
        // Create channels
        let (capture_tx, capture_rx) = unbounded_channel::<Vec<i16>>();
        let (playback_tx, playback_rx) = unbounded_channel::<Vec<i16>>();
        let (stt_bytes_tx, stt_bytes_rx) = unbounded_channel::<Vec<u8>>();

        // Create STT handle
        let stt_config = SttConfig::new(STT_ENDPOINT.to_string(), self.api_key.clone());
        let stt = Arc::new(SttHandle::new(stt_config));

        // Create TTS handle
        let tts_config = TtsConfig::new(
            TTS_ENDPOINT.to_string(),
            DEFAULT_VOICE_ID.to_string(),
            self.api_key.clone(),
        );
        let tts = Arc::new(TtsHandle::new(tts_config));

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

        // Spawn background tasks
        self.spawn_stt_ping_task(Arc::clone(&stt));
        self.spawn_tts_ping_task(Arc::clone(&tts));
        self.spawn_capture_task(stt_bytes_tx, capture_rx);
        self.spawn_stt_sender_task(Arc::clone(&stt), stt_bytes_rx);
        self.spawn_stt_event_task(Arc::clone(&stt), Arc::clone(&tts));
        self.spawn_tts_event_task(Arc::clone(&tts), playback_tx);

        // Store handles
        self.stt = Some(stt);
        self.tts = Some(tts);
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
                debug!("sending STT ping");
                if let Err(e) = stt.send_ping().await {
                    warn!("STT ping error: {e}");
                    break;
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
                debug!("sending TTS ping");
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
                if let Some(pcm_48k) = capture_rx.recv().await {
                    // Log periodically
                    audio_chunks_sent += 1;
                    if audio_chunks_sent % 100 == 1 {
                        debug!("audio capture: chunk #{}, {} samples", audio_chunks_sent, pcm_48k.len());
                    }

                    // Convert to bytes (little-endian i16)
                    let bytes: Vec<u8> = pcm_48k.iter().flat_map(|s| s.to_le_bytes()).collect();
                    let bytes_24k = downsample_48_to_24(&bytes);

                    // Send to STT bytes queue
                    if let Err(e) = stt_bytes_tx.send(bytes_24k) {
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
                            debug!("STT sender: chunk #{}, {} bytes", chunks_sent, chunk.len());
                        }

                        if let Err(e) = stt.process(&b64).await {
                            warn!("STT process error: {e}");
                        }
                    }
                } else {
                    // Channel closed, flush remaining buffer
                    if !buffer.is_empty() {
                        debug!("STT sender: flushing {} remaining bytes", buffer.len());
                        let b64 = BASE64.encode(&buffer);
                        if let Err(e) = stt.process(&b64).await {
                            warn!("STT process error on flush: {e}");
                        }
                    }
                    info!("STT sender task stopping");
                    break;
                }
            }
        });
    }

    fn spawn_stt_event_task(&self, stt: Arc<SttHandle>, tts: Arc<TtsHandle>) {
        let wg_guard = self.wg.add();
        tokio::spawn(async move {
            let _wg_guard = wg_guard;
            let mut pending_text = String::new();
            loop {
                debug!("STT event loop");
                match stt.next_event().await {
                    Ok(Some(event)) => {
                        match event {
                            SttEvent::Step { vad, .. } => {
                                if vad.len() > 2 && vad[2].inactivity_prob > 0.5 {
                                    debug!("user inactive (prob: {:.2})", vad[2].inactivity_prob);

                                    if !pending_text.trim().is_empty() {
                                        info!("Sending to TTS: '{}'", pending_text.trim());
                                        if let Err(e) = tts.process(pending_text.trim()).await {
                                            warn!("TTS process error: {e}");
                                        } else {
                                            info!("Sending EOS to TTS");
                                            if let Err(e) = tts.send_eos().await {
                                                warn!("TTS send eos error: {e}");
                                            }
                                        }
                                        pending_text.clear();
                                    }
                                }
                            }
                            SttEvent::Text { text, start } => {
                                info!("STT text: '{text}' at {start:.2}s");
                                if !text.trim().is_empty() {
                                    pending_text.push(' ');
                                    pending_text.push_str(&text);
                                }
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
                            SttEvent::EndOfStream => {
                                info!("STT end of stream");
                                break;
                            }
                            SttEvent::Ping => {
                                debug!("STT ping");
                            }
                            SttEvent::Pong => {
                                debug!("STT pong");
                            }
                            SttEvent::Close => {
                                warn!("STT connection closed");
                                break;
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

    fn spawn_tts_event_task(&self, tts: Arc<TtsHandle>, playback_tx: UnboundedSender<Vec<i16>>) {
        let wg_guard = self.wg.add();
        tokio::spawn(async move {
            let _wg_guard = wg_guard;
            loop {
                debug!("TTS event loop");
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
                            }
                            TtsEvent::EndOfStream => {
                                info!("TTS end of stream");
                                if tts.is_final_shutdown() {
                                    info!("TTS final shutdown");
                                    break;
                                }

                                if let Err(e) = tts.reconnect().await {
                                    error!("TTS reconnect error: {e}");
                                    break;
                                }
                            }
                            TtsEvent::Ping => {
                                debug!("TTS ping");
                            }
                            TtsEvent::Pong => {
                                debug!("TTS pong");
                            }
                            TtsEvent::Close => {
                                warn!("TTS connection closed");
                                break;
                            }
                            TtsEvent::Frame => {
                                debug!("TTS frame");
                            }
                        }
                    }
                    Ok(None) => {
                        error!("TTS client not connected");
                        break;
                    }
                    Err(e) => {
                        error!("TTS event error: {e}");
                        break;
                    }
                }
            }
        });
    }
}
