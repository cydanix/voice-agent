use rust_gradium::{SttClient, SttConfig, SttEvent};
use std::sync::{atomic::AtomicBool, Arc, atomic::Ordering};
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::RwLock;
use tokio::sync::Mutex;
use tracing::{error, info};
use std::collections::VecDeque;

#[derive(Error, Debug)]
pub enum SttHandleError {
    #[error("STT client not connected")]
    NotConnected,
    #[error("STT audio queue is full")]
    AudioQueueFull,
}

pub struct SttHandle {
    client: Arc<RwLock<Option<SttClient>>>,
    config: SttConfig,
    last_ping: Arc<RwLock<Instant>>,
    final_shutdown: AtomicBool,
    audio_queue: Mutex<VecDeque<String>>,
}

impl SttHandle {
    pub fn new(config: SttConfig) -> Self {
        Self {
            client: Arc::new(RwLock::new(None)),
            config,
            last_ping: Arc::new(RwLock::new(Instant::now())),
            final_shutdown: AtomicBool::new(false),
            audio_queue: Mutex::new(VecDeque::new()),
        }
    }

    pub async fn connect(&self) -> anyhow::Result<()> {
        let client = SttClient::new(self.config.clone());
        client.start().await?;
        *self.client.write().await = Some(client);
        *self.last_ping.write().await = Instant::now();
        Ok(())
    }

    pub async fn reconnect(&self) -> anyhow::Result<()> {
        info!("STT reconnecting...");
        if let Some(client) = self.client.write().await.take() {
            client.shutdown().await;
        }
        if let Err(e) = self.connect().await {
            error!("STT reconnect error: {e}");
            return Err(e);
        }
        self.process_queue().await?;
        Ok(())
    }

    async fn process_queue(&self) -> anyhow::Result<()> {
        if let Some(client) = self.client.read().await.as_ref() {
            loop {
                let mut audio_queue = self.audio_queue.lock().await;
                if audio_queue.is_empty() {
                    break;
                }
                let audio = audio_queue.pop_front().unwrap();
                if let Err(e) = client.process(&audio).await {
                    error!("TTS process error: {e}");
                    audio_queue.push_front(audio);
                    break;
                }
            }
        }
        Ok(())
    }

    async fn enqueue(&self, audio: &str) -> anyhow::Result<()> {
        let mut audio_queue = self.audio_queue.lock().await;
        if audio_queue.len() > 32 {
            return Err(SttHandleError::AudioQueueFull.into());
        }
        audio_queue.push_back(audio.to_string());
        Ok(())
    }

    pub async fn process(&self, audio: &str) -> anyhow::Result<()> {
        self.enqueue(audio).await?;
        self.process_queue().await?;
        Ok(())
    }

    pub async fn next_event(&self) -> anyhow::Result<Option<SttEvent>> {
        if let Some(ref client) = *self.client.read().await {
            match client.next_event().await {
                Ok(event) => {
                    *self.last_ping.write().await = Instant::now();
                    Ok(Some(event))
                }
                Err(e) => Err(e.into()),
            }
        } else {
            Err(SttHandleError::NotConnected.into())
        }
    }

    #[allow(dead_code)]
    pub async fn send_eos(&self) -> anyhow::Result<()> {
        if let Some(ref client) = *self.client.read().await {
            client.send_eos().await?;
            Ok(())
        } else {
            Err(SttHandleError::NotConnected.into())
        }
    }

    pub async fn send_ping(&self) -> anyhow::Result<()> {
        if let Some(ref client) = *self.client.read().await {
            client.send_ping().await?;
            Ok(())
        } else {
            Err(SttHandleError::NotConnected.into())
        }
    }

    pub fn set_final_shutdown(&self) {
        info!("STT: setting final shutdown");
        self.final_shutdown.store(true, Ordering::SeqCst);
    }

    pub fn is_final_shutdown(&self) -> bool {
        self.final_shutdown.load(Ordering::SeqCst)
    }

    pub async fn shutdown(&self) {
        if let Some(client) = self.client.write().await.take() {
            let _ = client.send_eos().await;
            client.shutdown().await;
        }
    }

    #[allow(dead_code)]
    pub async fn time_since_last_ping(&self) -> Duration {
        self.last_ping.read().await.elapsed()
    }
}

