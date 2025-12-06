use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleRate, StreamConfig};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, debug, warn};

pub struct PcmCapture {
    stream: cpal::Stream,
}

impl PcmCapture {
    pub fn new(tx: UnboundedSender<Vec<i16>>) -> Result<Self> {
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or(anyhow::anyhow!("no input device"))?;
        let supported = device.default_input_config()?;
        let sample_format = supported.sample_format();

        let config = StreamConfig {
            channels: 1,
            sample_rate: SampleRate(48000),
            buffer_size: cpal::BufferSize::Default,
        };

        let stream = match sample_format {
            cpal::SampleFormat::I16 => Self::build::<i16>(&device, &config, tx)?,
            cpal::SampleFormat::F32 => Self::build::<f32>(&device, &config, tx)?,
            cpal::SampleFormat::U16 => Self::build::<u16>(&device, &config, tx)?,
            _ => unreachable!(),
        };

        Ok(Self { stream })
    }

    fn build<T>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        tx: UnboundedSender<Vec<i16>>,
    ) -> Result<cpal::Stream>
    where
        T: cpal::Sample + cpal::SizedSample + Send + 'static,
        i16: FromSample<T>,
    {
        let err_fn = |e| error!("capture stream error: {e}");
        debug!("building input stream with config: {:?}", config);
        let stream = device.build_input_stream(
            config,
            move |data: &[T], _| {
                let pcm: Vec<i16> = data.iter().map(|s| i16::from_sample(*s)).collect();
                if let Err(e) = tx.send(pcm) {
                    warn!("failed to send captured pcm data: {e}");
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
}