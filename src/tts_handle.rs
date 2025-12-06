use rust_gradium::{TtsClient, TtsConfig, TtsEvent};
use std::sync::{atomic::AtomicBool, Arc, atomic::Ordering};
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};
use tracing::{error, info};

use std::collections::VecDeque;

#[derive(Error, Debug)]
pub enum TtsHandleError {
    #[error("TTS client not connected")]
    NotConnected,
    #[error("TTS text queue is full")]
    TextQueueFull,
}

pub struct TtsHandle {
    client: Arc<RwLock<Option<TtsClient>>>,
    config: TtsConfig,
    last_ping: Arc<RwLock<Instant>>,
    final_shutdown: AtomicBool,
    text_queue: Mutex<VecDeque<String>>,
}

impl TtsHandle {
    pub fn new(config: TtsConfig) -> Self {
        Self {
            client: Arc::new(RwLock::new(None)),
            config,
            last_ping: Arc::new(RwLock::new(Instant::now())),
            final_shutdown: AtomicBool::new(false),
            text_queue: Mutex::new(VecDeque::new()),
        }
    }

    pub async fn connect(&self) -> anyhow::Result<()> {
        let client = TtsClient::new(self.config.clone());
        client.start().await?;
        *self.client.write().await = Some(client);
        *self.last_ping.write().await = Instant::now();
        Ok(())
    }

    pub async fn reconnect(&self) -> anyhow::Result<()> {
        info!("TTS reconnecting...");
        if let Some(client) = self.client.write().await.take() {
            client.shutdown().await;
        }
        if let Err(e) = self.connect().await {
            error!("TTS reconnect error: {e}");
            return Err(e);
        }
        self.process_queue().await?;
        Ok(())
    }

    async fn process_queue(&self) -> anyhow::Result<()> {
        if let Some(client) = self.client.read().await.as_ref() {
            loop {
                let mut text_queue = self.text_queue.lock().await;
                if text_queue.is_empty() {
                    break;
                }
                let text = text_queue.pop_front().unwrap();
                if let Err(e) = client.process(&text).await {
                    error!("TTS process error: {e}");
                    text_queue.push_front(text);
                    break;
                }
            }
        }
        Ok(())
    }

    async fn enqueue(&self, text: &str) -> anyhow::Result<()> {
        let mut text_queue = self.text_queue.lock().await;
        if text_queue.len() > 32 {
            return Err(TtsHandleError::TextQueueFull.into());
        }
        text_queue.push_back(text.to_string());
        Ok(())
    }

    pub async fn process(&self, text: &str) -> anyhow::Result<()> {
        self.enqueue(text).await?;
        self.process_queue().await?;
        Ok(())
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

