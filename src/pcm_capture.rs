use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleRate, StreamConfig};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, info};

pub struct PcmCapture {
    stream: cpal::Stream,
    stopped: Arc<AtomicBool>,
    tx: Arc<Mutex<Option<UnboundedSender<Vec<i16>>>>>,
}

impl PcmCapture {
    pub fn new(tx: UnboundedSender<Vec<i16>>) -> Result<Self> {
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or(anyhow::anyhow!("no input device"))?;
        let device_name = device.name().unwrap_or_else(|_| "unknown".to_string());
        info!("audio capture using device: {}", device_name);
        let supported = device.default_input_config()?;
        info!("device default config: {:?}", supported);
        let sample_format = supported.sample_format();

        let config = StreamConfig {
            channels: 1,
            sample_rate: SampleRate(48000),
            buffer_size: cpal::BufferSize::Default,
        };

        let stopped = Arc::new(AtomicBool::new(false));
        let tx = Arc::new(Mutex::new(Some(tx)));

        let stream = match sample_format {
            cpal::SampleFormat::I16 => Self::build::<i16>(&device, &config, Arc::clone(&tx), Arc::clone(&stopped))?,
            cpal::SampleFormat::F32 => Self::build::<f32>(&device, &config, Arc::clone(&tx), Arc::clone(&stopped))?,
            cpal::SampleFormat::U16 => Self::build::<u16>(&device, &config, Arc::clone(&tx), Arc::clone(&stopped))?,
            _ => unreachable!(),
        };

        Ok(Self { stream, stopped, tx })
    }

    fn build<T>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        tx: Arc<Mutex<Option<UnboundedSender<Vec<i16>>>>>,
        stopped: Arc<AtomicBool>,
    ) -> Result<cpal::Stream>
    where
        T: cpal::Sample + cpal::SizedSample + Send + 'static,
        i16: FromSample<T>,
    {
        let err_fn = |e| error!("capture stream error: {e}");
        info!("building input stream with config: {:?}", config);
        let stream = device.build_input_stream(
            config,
            move |data: &[T], _| {
                if stopped.load(Ordering::Relaxed) {
                    return;
                }
                let pcm: Vec<i16> = data.iter().map(|s| i16::from_sample(*s)).collect();
                if let Ok(guard) = tx.lock() {
                    if let Some(ref sender) = *guard {
                        let _ = sender.send(pcm);
                    }
                }
            },
            err_fn,
            None,
        )?;
        Ok(stream)
    }

    pub fn start(&self) -> Result<()> {
        self.stream.play()?;
        Ok(())
    }

    pub fn stop(&self) {
        info!("stopping audio capture");
        self.stopped.store(true, Ordering::Relaxed);
        // Drop the sender to close the channel - this unblocks recv().await
        if let Ok(mut guard) = self.tx.lock() {
            *guard = None;
        }
        let _ = self.stream.pause();
    }
}
