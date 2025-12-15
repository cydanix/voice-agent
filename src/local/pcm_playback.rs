use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleRate, StreamConfig};
use tokio::sync::mpsc::UnboundedReceiver;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tracing::{error, debug, info};

use voice_agent::messages::AudioPlaybackMessage;

pub struct PcmPlayback {
    stream: cpal::Stream,
    queue: Arc<Mutex<VecDeque<i16>>>,
    stopped: Arc<AtomicBool>,
}

impl PcmPlayback {
    pub fn new(mut rx: UnboundedReceiver<AudioPlaybackMessage>) -> Result<Self> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(anyhow::anyhow!("no output device"))?;

        let config = StreamConfig {
            channels: 1,
            sample_rate: SampleRate(48000),
            buffer_size: cpal::BufferSize::Default,
        };

        let queue = Arc::new(Mutex::new(VecDeque::<i16>::new()));
        let stopped = Arc::new(AtomicBool::new(false));

        let queue_clone = Arc::clone(&queue);
        let stopped_clone = Arc::clone(&stopped);

        // background async consumer
        tokio::spawn(async move {
            debug!("playback consumer task started");
            while let Some(message) = rx.recv().await {
                if stopped_clone.load(Ordering::Relaxed) {
                    continue;
                }
                match message {
                    AudioPlaybackMessage::Play(chunk) => {
                        match queue_clone.lock() {
                            Ok(mut buf) => buf.extend(chunk),
                            Err(e) => error!("failed to lock playback queue: {e}"),
                        }
                    }
                    AudioPlaybackMessage::Reset => {
                        match queue_clone.lock() {
                            Ok(mut buf) => buf.clear(),
                            Err(e) => error!("failed to lock playback queue: {e}"),
                        }
                    }
                }
            }
            info!("playback consumer task ended");
        });

        let queue_clone2 = Arc::clone(&queue);
        let stopped_clone2 = Arc::clone(&stopped);

        debug!("building output stream with config: {:?}", config);
        let stream = device.build_output_stream(
            &config,
            move |out: &mut [i16], _| {
                if stopped_clone2.load(Ordering::Relaxed) {
                    for s in out {
                        *s = 0;
                    }
                    return;
                }
                match queue_clone2.lock() {
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


        Ok(Self { stream, queue, stopped })
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
