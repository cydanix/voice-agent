use rust_gradium::{TtsClient, TtsConfig, TtsEvent};
use std::sync::{atomic::AtomicBool, Arc, atomic::Ordering};
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};
use tracing::{error, info};

use std::sync::atomic::AtomicUsize;
use std::collections::VecDeque;

#[derive(Error, Debug)]
pub enum TtsHandleError {
    #[error("TTS client not connected")]
    NotConnected,
    #[error("TTS text queue is full")]
    TextQueueFull,
    #[error("TTS final shutdown")]
    FinalShutdown,
}

pub struct TtsHandle {
    client: Arc<RwLock<Option<TtsClient>>>,
    config: TtsConfig,
    last_ping: Arc<RwLock<Instant>>,
    final_shutdown: AtomicBool,
    text_queue: Mutex<VecDeque<String>>,
    inflight_requests: AtomicUsize,
    reconnecting: AtomicBool,
}

impl TtsHandle {
    const MAX_TEXT_QUEUE_SIZE: usize = 64;

    pub fn new(config: TtsConfig) -> Self {
        Self {
            client: Arc::new(RwLock::new(None)),
            config,
            last_ping: Arc::new(RwLock::new(Instant::now())),
            final_shutdown: AtomicBool::new(false),
            text_queue: Mutex::new(VecDeque::new()),
            inflight_requests: AtomicUsize::new(0),
            reconnecting: AtomicBool::new(false),
        }
    }

    pub async fn connect(&self) -> anyhow::Result<()> {
        let client = TtsClient::new(self.config.clone());
        client.start().await?;
        {
            let mut client_ptr = self.client.write().await;
            *client_ptr = Some(client);
            self.inflight_requests.store(0, Ordering::SeqCst);
        }
        *self.last_ping.write().await = Instant::now();
        Ok(())
    }

    pub async fn reconnect(&self) -> anyhow::Result<()> {
        info!("TTS reconnecting...");
        if self.is_final_shutdown() {
            info!("TTS final shutdown");
            return Err(TtsHandleError::FinalShutdown.into());
        }

        self.reconnecting.store(true, Ordering::SeqCst);

        if let Some(client) = self.client.write().await.take() {
            client.shutdown().await;
        }
        if let Err(e) = self.connect().await {
            error!("TTS reconnect error: {e}");
            return Err(e);
        }
        self.process_queue().await?;

        self.reconnecting.store(false, Ordering::SeqCst);

        info!("TTS reconnecting done");
        if self.is_final_shutdown() {
            info!("TTS final shutdown");
            return Err(TtsHandleError::FinalShutdown.into());
        }
        Ok(())
    }

    pub fn set_reconnecting(&self) {
        info!("TTS: setting reconnecting");
        self.reconnecting.store(true, Ordering::SeqCst);
    }

    pub fn is_reconnecting(&self) -> bool {
        self.reconnecting.load(Ordering::SeqCst)
    }

    pub async fn cancel(&self) -> anyhow::Result<()> {
        if self.inflight_requests.load(Ordering::SeqCst) > 0 {
            info!("TTS: cancelling inflight requests");
            self.text_queue.lock().await.clear();
            self.reconnect().await?;
        } else {
            info!("TTS: no inflight requests to cancel");
        }
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
                } else {
                    self.inflight_requests.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
        Ok(())
    }

    async fn enqueue(&self, text: &str) -> anyhow::Result<()> {
        let mut text_queue = self.text_queue.lock().await;
        if text_queue.len() > Self::MAX_TEXT_QUEUE_SIZE {
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
            tokio::select! {
                result = client.next_event() => {
                    match result {
                        Ok(event) => {
                            *self.last_ping.write().await = Instant::now();
                            Ok(Some(event))
                        }
                        Err(e) => Err(e.into()),
                    }
                }
                _ = async {
                    loop {
                        if self.is_final_shutdown() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                } => {
                    Err(TtsHandleError::FinalShutdown.into())
                }
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

    pub fn inflight_requests(&self) -> usize {
        self.inflight_requests.load(Ordering::SeqCst)
    }

    pub async fn sleep(&self, delay_ms: u64) -> anyhow::Result<()> {
        const ROUND_DELAY_MS: u64 = 100;
        for _ in 0..delay_ms/ROUND_DELAY_MS {
            tokio::time::sleep(Duration::from_millis(ROUND_DELAY_MS)).await;
            if self.is_final_shutdown() {
                return Err(TtsHandleError::FinalShutdown.into());
            }
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn time_since_last_ping(&self) -> Duration {
        self.last_ping.read().await.elapsed()
    }
}

