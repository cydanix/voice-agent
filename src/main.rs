mod downsampler;
mod pcm_capture;
mod pcm_playback;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rust_gradium::{SttClient, SttConfig, TtsClient, TtsConfig, STT_ENDPOINT, TTS_ENDPOINT, DEFAULT_VOICE_ID};
use tokio::sync::mpsc::unbounded_channel;
use tokio::signal::unix::{signal, SignalKind};
use pcm_capture::PcmCapture;
use pcm_playback::PcmPlayback;
use tracing::{info, error, debug, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::DEBUG.into()),
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
    let stt = SttClient::new(stt_config);

    // Create TTS client
    let tts_config = TtsConfig::new(
        TTS_ENDPOINT.to_string(),
        DEFAULT_VOICE_ID.to_string(),
        api_key,
    );
    let tts = TtsClient::new(tts_config);

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
    let capture_task = async {
        while let Some(pcm_48k) = capture_rx.recv().await {
            // Downsample 48kHz → 24kHz
            let pcm_24k = downsampler::downsample_48_to_24(&pcm_48k);

            // Convert to bytes (little-endian i16)
            let bytes: Vec<u8> = pcm_24k.iter().flat_map(|s| s.to_le_bytes()).collect();

            // Encode to base64
            let b64 = BASE64.encode(&bytes);

            // Send to STT
            if let Err(e) = stt_ref.process(&b64).await {
                warn!("STT process error: {e}");
            }
        }
    };

    // Task: get text from STT → send to TTS
    let tts_ref = &tts;
    let stt_to_tts_task = async {
        loop {
            let texts = stt_ref.get_text(100).await;
            for text in texts {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    info!("STT text: {trimmed}");
                    if let Err(e) = tts_ref.process(trimmed).await {
                        warn!("TTS process error: {e}");
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    };

    // Task: get audio from TTS → decode → send to playback
    let playback_tx_ref = &playback_tx;
    let tts_to_playback_task = async {
        loop {
            let chunks = tts_ref.get_speech(100).await;
            for b64_audio in chunks {
                match BASE64.decode(&b64_audio) {
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
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
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
        _ = stt_to_tts_task => {}
        _ = tts_to_playback_task => {}
        _ = shutdown_signal => {}
    }

    // Cleanup
    info!("shutting down STT and TTS");
    stt.shutdown().await;
    tts.shutdown().await;

    Ok(())
}
