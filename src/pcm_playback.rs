use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleRate, StreamConfig};
use tokio::sync::mpsc::UnboundedReceiver;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::sync::mpsc as std_mpsc;
use tracing::{error, debug, info};

enum PlaybackCommand {
    Stop,
    Start,
}

/// A Send-safe handle for controlling playback from async tasks
#[derive(Clone)]
pub struct PcmPlaybackController {
    command_tx: std_mpsc::Sender<PlaybackCommand>,
}

impl PcmPlaybackController {
    pub fn restart(&self) -> Result<()> {
        let _ = self.command_tx.send(PlaybackCommand::Stop);
        self.command_tx.send(PlaybackCommand::Start)
            .map_err(|_| anyhow::anyhow!("playback command channel closed"))
    }
}

pub struct PcmPlayback {
    stream: cpal::Stream,
    queue: Arc<Mutex<VecDeque<i16>>>,
    stopped: Arc<AtomicBool>,
    command_rx: std_mpsc::Receiver<PlaybackCommand>,
    controller: PcmPlaybackController,
}

impl PcmPlayback {
    pub fn new(mut rx: UnboundedReceiver<Vec<i16>>) -> Result<Self> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(anyhow::anyhow!("no output device"))?;

        let config = StreamConfig {
            channels: 1,
            sample_rate: SampleRate(48000),
            buffer_size: cpal::BufferSize::Default,
        };

        let queue = Arc::new(Mutex::new(VecDeque::<i16>::new()));
        let stopped = Arc::new(AtomicBool::new(false));

        let q_consumer = Arc::clone(&queue);
        let stopped_consumer = Arc::clone(&stopped);

        // background async consumer
        tokio::spawn(async move {
            debug!("playback consumer task started");
            while let Some(chunk) = rx.recv().await {
                if stopped_consumer.load(Ordering::Relaxed) {
                    break;
                }
                match q_consumer.lock() {
                    Ok(mut buf) => buf.extend(chunk),
                    Err(e) => error!("failed to lock playback queue: {e}"),
                }
            }
            info!("playback consumer task ended");
        });

        let q_callback = Arc::clone(&queue);
        let stopped_callback = Arc::clone(&stopped);

        debug!("building output stream with config: {:?}", config);
        let stream = device.build_output_stream(
            &config,
            move |out: &mut [i16], _| {
                if stopped_callback.load(Ordering::Relaxed) {
                    for s in out {
                        *s = 0;
                    }
                    return;
                }
                match q_callback.lock() {
                    Ok(mut buf) => {
                        for s in out {
                            *s = buf.pop_front().unwrap_or(0);
                        }
                    }
                    Err(e) => error!("failed to lock playback queue in callback: {e}"),
                }
            },
            |e| error!("playback stream error: {e}"),
            None,
        )?;

        let (command_tx, command_rx) = std_mpsc::channel();
        let controller = PcmPlaybackController { command_tx };

        Ok(Self { stream, queue, stopped, command_rx, controller })
    }

    /// Get a Send-safe controller for this playback
    pub fn controller(&self) -> PcmPlaybackController {
        self.controller.clone()
    }

    /// Process any pending commands (call this periodically from the main thread)
    pub fn process_commands(&self) {
        while let Ok(cmd) = self.command_rx.try_recv() {
            match cmd {
                PlaybackCommand::Stop => self.stop(),
                PlaybackCommand::Start => { let _ = self.start(); }
            }
        }
    }

    pub fn start(&self) -> Result<()> {
        self.stopped.store(false, Ordering::Relaxed);
        self.stream.play()?;
        Ok(())
    }

    pub fn stop(&self) {
        info!("stopping audio playback");
        self.stopped.store(true, Ordering::Relaxed);
        if let Ok(mut buf) = self.queue.lock() {
            buf.clear();
        }
        let _ = self.stream.pause();
    }
}
