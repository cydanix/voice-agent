mod downsampler;
mod pcm_capture;
mod pcm_playback;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rust_gradium::{SttClient, SttConfig, SttEvent, TtsClient, TtsConfig, TtsEvent, STT_ENDPOINT, TTS_ENDPOINT, DEFAULT_VOICE_ID};
use tokio::sync::mpsc::unbounded_channel;
use tokio::signal::unix::{signal, SignalKind};
use pcm_capture::PcmCapture;
use pcm_playback::PcmPlayback;
use tracing::{info, error, debug, warn};

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

    // Create STT client
    let stt_config = SttConfig::new(STT_ENDPOINT.to_string(), api_key.clone());
    let (stt, mut stt_events) = SttClient::new(stt_config);

    // Create TTS client
    let tts_config = TtsConfig::new(
        TTS_ENDPOINT.to_string(),
        DEFAULT_VOICE_ID.to_string(),
        api_key,
    );
    let (tts, mut tts_events) = TtsClient::new(tts_config);

    // Start STT
    if let Err(e) = stt.start().await {
        error!("failed to start STT: {e}");
        return Err(anyhow::anyhow!("STT start failed: {e}"));
    }
    info!("STT connected");

    // Start TTS
    if let Err(e) = tts.start().await {
        error!("failed to start TTS: {e}");
        stt.shutdown().await;
        return Err(anyhow::anyhow!("TTS start failed: {e}"));
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
    let stt_ref = &stt;
    let mut audio_chunks_sent = 0u64;
    let capture_task = async {
        while let Some(pcm_48k) = capture_rx.recv().await {
            // Downsample 48kHz → 24kHz
            let pcm_24k = downsampler::downsample_48_to_24(&pcm_48k);

            // Convert to bytes (little-endian i16)
            let bytes: Vec<u8> = pcm_24k.iter().flat_map(|s| s.to_le_bytes()).collect();

            // Encode to base64
            let b64 = BASE64.encode(&bytes);

            // Log periodically
            audio_chunks_sent += 1;
            if audio_chunks_sent % 100 == 1 {
                debug!("audio capture: chunk #{}, {} samples @ 48kHz → {} samples @ 24kHz, {} bytes",
                    audio_chunks_sent, pcm_48k.len(), pcm_24k.len(), bytes.len());
            }

            // Send to STT
            if let Err(e) = stt_ref.process(&b64).await {
                warn!("STT process error: {e}");
            }
        }
    };

    // Task: handle STT events → send text to TTS
    let tts_ref = &tts;
    let stt_event_task = async {
        let mut pending_text = String::new();
        while let Some(event) = stt_events.recv().await {
            match event {
                SttEvent::UserInactive { inactivity_prob } => {
                    info!("user inactive (prob: {inactivity_prob:.2})");
                    // Send accumulated text to TTS when user stops speaking
                    if !pending_text.trim().is_empty() {
                        info!("Sending to TTS: '{}'", pending_text.trim());
                        if let Err(e) = tts_ref.process(pending_text.trim()).await {
                            warn!("TTS process error: {e}");
                        }
                        pending_text.clear();
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
                    // Could also send text here on end of phrase
                }
                SttEvent::Ready { request_id, model_name, sample_rate } => {
                    info!("STT ready: request_id={request_id}, model={model_name}, sample_rate={sample_rate}");
                }
                SttEvent::Error { message, code } => {
                    error!("STT error: {message} (code: {code})");
                }
                SttEvent::EndOfStream => {
                    info!("STT end of stream");
                }
            }
        }
    };

    // Task: handle TTS events → decode audio → send to playback
    let playback_tx_ref = &playback_tx;
    let tts_event_task = async {
        while let Some(event) = tts_events.recv().await {
            match event {
                TtsEvent::Audio { audio } => {
                    match BASE64.decode(&audio) {
                        Ok(bytes) => {
                            // Convert bytes to i16 samples (little-endian)
                            let samples: Vec<i16> = bytes
                                .chunks_exact(2)
                                .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                                .collect();

                            if let Err(e) = playback_tx_ref.send(samples) {
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
                }
            }
        }
    };

    // Signal handlers
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    let shutdown_signal = async {
        tokio::select! {
            _ = sigterm.recv() => {
                info!("received SIGTERM, shutting down");
            }
            _ = sigint.recv() => {
                info!("received SIGINT (Ctrl-C), shutting down");
            }
        }
    };

    // Run all tasks until shutdown signal
    tokio::select! {
        _ = capture_task => {}
        _ = stt_event_task => {}
        _ = tts_event_task => {}
        _ = shutdown_signal => {}
    }

    // Cleanup
    info!("shutting down STT and TTS");
    stt.shutdown().await;
    tts.shutdown().await;

    Ok(())
}
