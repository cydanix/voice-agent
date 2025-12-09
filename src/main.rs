mod llm;
mod pcm_capture;
mod pcm_playback;
mod stt_handle;
mod tts_handle;
mod voice_agent;
mod pause_detector;

use tracing::{debug, error, info};
use voice_agent::{VoiceAgent, Config};

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
        .unwrap_or_else(|_| llm::endpoints::OPENAI.to_string());

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

    let mut agent = VoiceAgent::new(config);
    if let Err(e) = agent.start().await {
        error!("error starting voice agent: {e}");
        return Err(e);
    }
    info!("voice agent started");

    match agent.run().await {
        Ok(_) => {
            info!("run completed successfully");
            agent.shutdown().await;
        }
        Err(e) => {
            error!("error running voice agent: {e}");
            agent.shutdown().await;
        }
    }

    Ok(())
}
