use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleRate, StreamConfig};
use tokio::sync::mpsc::UnboundedReceiver;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tracing::{error, debug, warn};

pub struct PcmPlayback {
    stream: cpal::Stream,
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
        let q_clone = queue.clone();

        // background async consumer
        tokio::spawn(async move {
            debug!("playback consumer task started");
            while let Some(chunk) = rx.recv().await {
                match q_clone.lock() {
                    Ok(mut buf) => buf.extend(chunk),
                    Err(e) => error!("failed to lock playback queue: {e}"),
                }
            }
            warn!("playback consumer task ended (channel closed)");
        });

        debug!("building output stream with config: {:?}", config);
        let stream = device.build_output_stream(
            &config,
            move |out: &mut [i16], _| {
                match queue.lock() {
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

        Ok(Self { stream })
    }

    pub fn start(&self) -> Result<()> {
        self.stream.play()?;
        Ok(())
    }
}