
use tracing::{debug, error, info};
use voice_agent::voice_agent::{VoiceAgent, Config};
use crate::pcm_capture::{PcmCapture};
use crate::pcm_playback::{PcmPlayback};
use voice_agent::messages::{AudioCaptureMessage, AudioPlaybackMessage};
use tokio::sync::mpsc::unbounded_channel;
use tokio::signal::unix::{signal, SignalKind};

mod pcm_capture;
mod pcm_playback;


#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    debug!("starting voice-agent");

    // Read configuration from environment variables
    let gradium_api_key = std::env::var("GRADIUM_API_KEY")
        .map_err(|_| anyhow::anyhow!("GRADIUM_API_KEY environment variable not set"))?;

    let stt_endpoint = std::env::var("GRADIUM_STT_ENDPOINT")
        .unwrap_or_else(|_| rust_gradium::STT_ENDPOINT.to_string());

    let stt_language = std::env::var("GRADIUM_STT_LANGUAGE")
        .unwrap_or_else(|_| "en".to_string());

    let tts_endpoint = std::env::var("GRADIUM_TTS_ENDPOINT")
        .unwrap_or_else(|_| rust_gradium::TTS_ENDPOINT.to_string());
    let tts_voice_id = std::env::var("GRADIUM_TTS_VOICE_ID")
        .unwrap_or_else(|_| rust_gradium::DEFAULT_VOICE_ID.to_string());


    let llm_api_key = std::env::var("OPENAI_API_KEY")
        .or_else(|_| std::env::var("LLM_API_KEY"))
        .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY or LLM_API_KEY environment variable not set"))?;

    let llm_model = std::env::var("OPENAI_MODEL")
        .or_else(|_| std::env::var("LLM_MODEL"))
        .unwrap_or_else(|_| "gpt-4o-mini".to_string());

    let llm_endpoint = std::env::var("LLM_ENDPOINT")
        .unwrap_or_else(|_| voice_agent::llm::endpoints::OPENAI.to_string());

    let llm_system_prompt = std::env::var("LLM_SYSTEM_PROMPT")
        .unwrap_or_else(|_| "You are a helpful voice assistant. Keep your responses concise and conversational.".to_string());

    let config = Config {
        tts_api_key: gradium_api_key.clone(),
        tts_endpoint: tts_endpoint,
        stt_endpoint: stt_endpoint,
        stt_api_key: gradium_api_key,
        stt_language: stt_language,
        tts_voice_id: tts_voice_id,
        llm_api_key: llm_api_key,
        llm_model: llm_model,
        llm_endpoint: llm_endpoint,
        llm_system_prompt: llm_system_prompt,
    };

    let (capture_tx, capture_rx) = unbounded_channel::<AudioCaptureMessage>();
    let (playback_tx, playback_rx) = unbounded_channel::<AudioPlaybackMessage>();

    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    let capture = match PcmCapture::new(capture_tx) {
        Ok(c) => c,
        Err(e) => {
            error!("failed to create PcmCapture: {e}");
            return Err(e);
        }
    };
    let playback = match PcmPlayback::new(playback_rx) {
        Ok(p) => p,
        Err(e) => {
            error!("failed to create PcmPlayback: {e}");
            return Err(e);
        }
    };

    if let Err(e) = capture.start() {
        error!("failed to start capture: {e}");
        return Err(e);
    }
    if let Err(e) = playback.start() {
        error!("failed to start playback: {e}");
        capture.stop();
        return Err(e);
    }

    let mut agent = VoiceAgent::new(config);
    if let Err(e) = agent.start(capture_rx, playback_tx).await {
        error!("error starting voice agent: {e}");
        capture.stop();
        playback.stop();
        return Err(e);
    }
    info!("voice agent started");

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                info!("received SIGTERM, shutting down");
                agent.set_stopping();
                break;
            }
            _ = sigint.recv() => {
                info!("received SIGINT (Ctrl-C), shutting down");
                agent.set_stopping();
                break;
            }
            _ = agent.run() => {
                info!("voice agent run completed");
                break;
            }
        }
    }
    capture.stop();
    playback.stop();
    agent.shutdown().await;

    Ok(())
}
