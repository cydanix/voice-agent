mod llm;
mod pcm_capture;
mod pcm_playback;
mod stt_handle;
mod tts_handle;
mod voice_agent;

use tracing::{debug, error, info};
use voice_agent::VoiceAgent;

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

    let mut agent = VoiceAgent::new(api_key);
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
