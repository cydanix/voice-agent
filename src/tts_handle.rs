use rust_gradium::{TtsClient, TtsConfig, TtsEvent};
use std::sync::{atomic::AtomicBool, Arc, atomic::Ordering};
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::info;

#[derive(Error, Debug)]
pub enum TtsHandleError {
    #[error("TTS client not connected")]
    NotConnected,
}

pub struct TtsHandle {
    client: Arc<RwLock<Option<TtsClient>>>,
    config: TtsConfig,
    last_ping: Arc<RwLock<Instant>>,
    final_shutdown: AtomicBool,
}

impl TtsHandle {
    pub fn new(config: TtsConfig) -> Self {
        Self {
            client: Arc::new(RwLock::new(None)),
            config,
            last_ping: Arc::new(RwLock::new(Instant::now())),
            final_shutdown: AtomicBool::new(false),
        }
    }

    pub async fn connect(&self) -> anyhow::Result<()> {
        let client = TtsClient::new(self.config.clone());
        client.start().await?;
        *self.client.write().await = Some(client);
        *self.last_ping.write().await = Instant::now();
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn reconnect(&self) -> anyhow::Result<()> {
        info!("TTS reconnecting...");
        if let Some(client) = self.client.write().await.take() {
            client.shutdown().await;
        }
        self.connect().await
    }

    pub async fn process(&self, text: &str) -> anyhow::Result<()> {
        if let Some(ref client) = *self.client.read().await {
            client.process(text).await?;
            Ok(())
        } else {
            Err(TtsHandleError::NotConnected.into())
        }
    }

    pub async fn next_event(&self) -> anyhow::Result<Option<TtsEvent>> {
        if let Some(ref client) = *self.client.read().await {
            match client.next_event().await {
                Ok(event) => {
                    *self.last_ping.write().await = Instant::now();
                    Ok(Some(event))
                }
                Err(e) => Err(e.into()),
            }
        } else {
            Err(TtsHandleError::NotConnected.into())
        }
    }

    pub async fn send_ping(&self) -> anyhow::Result<()> {
        if let Some(ref client) = *self.client.read().await {
            client.send_ping().await?;
            Ok(())
        } else {
            Err(TtsHandleError::NotConnected.into())
        }
    }

    #[allow(dead_code)]
    pub async fn send_eos(&self) -> anyhow::Result<()> {
        if let Some(ref client) = *self.client.read().await {
            client.send_eos().await?;
            Ok(())
        } else {
            Err(TtsHandleError::NotConnected.into())
        }
    }

    pub async fn shutdown(&self) {
        if let Some(client) = self.client.write().await.take() {
            let _ = client.send_eos().await;
            client.shutdown().await;
        }
    }

    pub fn set_final_shutdown(&self) {
        info!("TTS: setting final shutdown");
        self.final_shutdown.store(true, Ordering::SeqCst);
    }

    pub fn is_final_shutdown(&self) -> bool {
        self.final_shutdown.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    pub async fn time_since_last_ping(&self) -> Duration {
        self.last_ping.read().await.elapsed()
    }
}

