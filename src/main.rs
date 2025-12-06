mod pcm_capture;
mod pcm_playback;

use tokio::sync::mpsc::unbounded_channel;
use tokio::signal::unix::{signal, SignalKind};
use pcm_capture::PcmCapture;
use pcm_playback::PcmPlayback;
use tracing::{info, error, debug};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    debug!("starting voice-agent");

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::DEBUG.into()),
        )
        .init();

    let (tx, rx) = unbounded_channel();

    let capt = match PcmCapture::new(tx) {
        Ok(c) => c,
        Err(e) => {
            error!("failed to create PcmCapture: {e}");
            return Err(e);
        }
    };

    let play = match PcmPlayback::new(rx) {
        Ok(p) => p,
        Err(e) => {
            error!("failed to create PcmPlayback: {e}");
            return Err(e);
        }
    };

    if let Err(e) = capt.start() {
        error!("failed to start capture: {e}");
        return Err(e);
    }

    if let Err(e) = play.start() {
        error!("failed to start playback: {e}");
        return Err(e);
    }

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