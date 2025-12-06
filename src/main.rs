mod pcm_capture;
mod pcm_playback;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rust_gradium::{
    downsample_48_to_24_base64, SttClient, SttConfig, SttEvent, TtsClient, TtsConfig, TtsEvent,
    STT_ENDPOINT, TTS_ENDPOINT, DEFAULT_VOICE_ID,
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::RwLock;
use tokio::signal::unix::{signal, SignalKind};
use pcm_capture::PcmCapture;
use pcm_playback::PcmPlayback;
use tracing::{info, error, debug, warn};

/// Wrapper for STT client with reconnection support
struct SttHandle {
    client: Arc<RwLock<Option<SttClient>>>,
    config: SttConfig,
    last_ping: Arc<RwLock<Instant>>,
}

impl SttHandle {
    fn new(config: SttConfig) -> Self {
        Self {
            client: Arc::new(RwLock::new(None)),
            config,
            last_ping: Arc::new(RwLock::new(Instant::now())),
        }
    }

    async fn connect(&self) -> anyhow::Result<()> {
        let client = SttClient::new(self.config.clone());
        client.start().await?;
        *self.client.write().await = Some(client);
        *self.last_ping.write().await = Instant::now();
        Ok(())
    }

    async fn reconnect(&self) -> anyhow::Result<()> {
        info!("STT reconnecting...");
        // Shutdown old client if exists
        if let Some(client) = self.client.write().await.take() {
            client.shutdown().await;
        }
        self.connect().await
    }

    async fn process(&self, audio: &str) -> anyhow::Result<()> {
        if let Some(ref client) = *self.client.read().await {
            client.process(audio).await?;
        }
        Ok(())
    }

    async fn next_event(&self) -> anyhow::Result<Option<SttEvent>> {
        if let Some(ref client) = *self.client.read().await {
            match client.next_event().await {
                Ok(event) => {
                    // Update ping time on any event
                    *self.last_ping.write().await = Instant::now();
                    Ok(Some(event))
                }
                Err(e) => Err(e.into()),
            }
        } else {
            Ok(None)
        }
    }

    async fn shutdown(&self) {
        if let Some(client) = self.client.write().await.take() {
            let _ = client.send_eos().await;
            client.shutdown().await;
        }
    }

    async fn time_since_last_ping(&self) -> Duration {
        self.last_ping.read().await.elapsed()
    }
}

/// Wrapper for TTS client with reconnection support
struct TtsHandle {
    client: Arc<RwLock<Option<TtsClient>>>,
    config: TtsConfig,
    last_ping: Arc<RwLock<Instant>>,
}

impl TtsHandle {
    fn new(config: TtsConfig) -> Self {
        Self {
            client: Arc::new(RwLock::new(None)),
            config,
            last_ping: Arc::new(RwLock::new(Instant::now())),
        }
    }

    async fn connect(&self) -> anyhow::Result<()> {
        let client = TtsClient::new(self.config.clone());
        client.start().await?;
        *self.client.write().await = Some(client);
        *self.last_ping.write().await = Instant::now();
        Ok(())
    }

    async fn reconnect(&self) -> anyhow::Result<()> {
        info!("TTS reconnecting...");
        // Shutdown old client if exists
        if let Some(client) = self.client.write().await.take() {
            client.shutdown().await;
        }
        self.connect().await
    }

    async fn process(&self, text: &str) -> anyhow::Result<()> {
        if let Some(ref client) = *self.client.read().await {
            client.process(text).await?;
        }
        Ok(())
    }

    async fn next_event(&self) -> anyhow::Result<Option<TtsEvent>> {
        if let Some(ref client) = *self.client.read().await {
            match client.next_event().await {
                Ok(event) => {
                    // Update ping time on any event
                    *self.last_ping.write().await = Instant::now();
                    Ok(Some(event))
                }
                Err(e) => Err(e.into()),
            }
        } else {
            Ok(None)
        }
    }

    async fn shutdown(&self) {
        if let Some(client) = self.client.write().await.take() {
            let _ = client.send_eos().await;
            client.shutdown().await;
        }
    }

    async fn time_since_last_ping(&self) -> Duration {
        self.last_ping.read().await.elapsed()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    debug!("starting voice-agent");

    let api_key = std::env::var("GRADIUM_API_KEY")
        .map_err(|_| anyhow::anyhow!("GRADIUM_API_KEY environment variable not set"))?;

    // Create channels
    let (capture_tx, mut capture_rx) = unbounded_channel::<Vec<i16>>();
    let (playback_tx, playback_rx) = unbounded_channel::<Vec<i16>>();

    // Create STT handle with reconnection support
    let stt_config = SttConfig::new(STT_ENDPOINT.to_string(), api_key.clone());
    let stt = Arc::new(SttHandle::new(stt_config));

    // Create TTS handle with reconnection support
    let tts_config = TtsConfig::new(
        TTS_ENDPOINT.to_string(),
        DEFAULT_VOICE_ID.to_string(),
        api_key,
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

    // Create audio capture and playback
    let capt = match PcmCapture::new(capture_tx) {
        Ok(c) => c,
        Err(e) => {
            error!("failed to create PcmCapture: {e}");
            stt.shutdown().await;
            tts.shutdown().await;
            return Err(e);
        }
    };

    let play = match PcmPlayback::new(playback_rx) {
        Ok(p) => p,
        Err(e) => {
            error!("failed to create PcmPlayback: {e}");
            stt.shutdown().await;
            tts.shutdown().await;
            return Err(e);
        }
    };

    if let Err(e) = capt.start() {
        error!("failed to start capture: {e}");
        stt.shutdown().await;
        tts.shutdown().await;
        return Err(e);
    }

    if let Err(e) = play.start() {
        error!("failed to start playback: {e}");
        stt.shutdown().await;
        tts.shutdown().await;
        return Err(e);
    }

    info!("running… press Ctrl-C or send SIGTERM to stop");

    // Task: capture audio → downsample → send to STT
    let stt_capture = Arc::clone(&stt);
    let capture_task = tokio::spawn(async move {
        let mut audio_chunks_sent = 0u64;
        while let Some(pcm_48k) = capture_rx.recv().await {
            // Convert to bytes (little-endian i16)
            let bytes: Vec<u8> = pcm_48k.iter().flat_map(|s| s.to_le_bytes()).collect();
            let b64_48k = BASE64.encode(&bytes);

            // Downsample 48kHz → 24kHz using library function
            let b64_24k = downsample_48_to_24_base64(&b64_48k);

            // Log periodically
            audio_chunks_sent += 1;
            if audio_chunks_sent % 100 == 1 {
                debug!("audio capture: chunk #{}, {} samples", audio_chunks_sent, pcm_48k.len());
            }

            // Send to STT
            if let Err(e) = stt_capture.process(&b64_24k).await {
                warn!("STT process error: {e}");
            }
        }
    });

    // Task: handle STT events → send text to TTS
    let stt_events = Arc::clone(&stt);
    let tts_for_stt = Arc::clone(&tts);
    let stt_event_task = tokio::spawn(async move {
        let mut pending_text = String::new();
        loop {
            match stt_events.next_event().await {
                Ok(Some(event)) => {
                    match event {
                        SttEvent::Step { vad, .. } => {
                            // Check for user inactivity via VAD
                            if vad.len() > 2 && vad[2].inactivity_prob > 0.5 {
                                debug!("user inactive (prob: {:.2})", vad[2].inactivity_prob);
                                // Send accumulated text to TTS when user stops speaking
                                if !pending_text.trim().is_empty() {
                                    info!("Sending to TTS: '{}'", pending_text.trim());
                                    if let Err(e) = tts_for_stt.process(pending_text.trim()).await {
                                        warn!("TTS process error: {e}");
                                    }
                                    pending_text.clear();
                                }
                            }
                        }
                        SttEvent::Text { text, start } => {
                            info!("STT text: '{text}' at {start:.2}s");
                            // Accumulate recognized text
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
                    // Client not connected
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => {
                    error!("STT event error: {e}");
                    break;
                }
            }
        }
    });

    // Task: handle TTS events → decode audio → send to playback
    let tts_events = Arc::clone(&tts);
    let tts_event_task = tokio::spawn(async move {
        loop {
            match tts_events.next_event().await {
                Ok(Some(event)) => {
                    match event {
                        TtsEvent::Audio { audio } => {
                            match BASE64.decode(&audio) {
                                Ok(bytes) => {
                                    // Convert bytes to i16 samples (little-endian)
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
                            break;
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
                    // Client not connected
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => {
                    error!("TTS event error: {e}");
                    break;
                }
            }
        }
    });

    // Wait for shutdown signal
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

    // Graceful shutdown
    info!("shutting down STT and TTS");
    stt.shutdown().await;
    tts.shutdown().await;

    // Wait for event tasks to finish
    let _ = tokio::time::timeout(Duration::from_secs(5), stt_event_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), tts_event_task).await;

    // Abort capture task
    capture_task.abort();

    info!("shutdown complete");
    Ok(())
}
