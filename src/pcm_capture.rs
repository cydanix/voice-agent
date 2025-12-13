use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleRate, StreamConfig};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, info};


pub enum PcmCaptureMessage {
    Chunk(Vec<i16>),
}

pub struct PcmCapture {
    stream: cpal::Stream,
    stopped: Arc<AtomicBool>,
    tx: Arc<Mutex<Option<UnboundedSender<PcmCaptureMessage>>>>,
}

impl PcmCapture {
    pub fn new(tx: UnboundedSender<PcmCaptureMessage>) -> Result<Self> {
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or(anyhow::anyhow!("no input device"))?;
        let device_name = device.name().unwrap_or_else(|_| "unknown".to_string());
        info!("audio capture using device: {}", device_name);
        let supported = device.default_input_config()?;
        info!("device default config: {:?}", supported);
        let sample_format = supported.sample_format();
        let device_sample_rate = supported.sample_rate().0;

        // Choose sample rate: use device's native rate if it's 16k, 24k, or 48k
        let sample_rate = match device_sample_rate {
            16000 | 24000 | 48000 => device_sample_rate,
            _ => {
                // Try to find a supported rate from our preferred list
                if Self::supports_sample_rate(&device, 48000) {
                    48000
                } else if Self::supports_sample_rate(&device, 24000) {
                    24000
                } else if Self::supports_sample_rate(&device, 16000) {
                    16000
                } else {
                    // Fall back to device's native rate and hope for the best
                    device_sample_rate
                }
            }
        };

        info!("using sample rate: {}Hz (will resample to 24kHz)", sample_rate);

        let config = StreamConfig {
            channels: 1,
            sample_rate: SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let stopped = Arc::new(AtomicBool::new(false));
        let tx = Arc::new(Mutex::new(Some(tx)));

        let stream = match sample_format {
            cpal::SampleFormat::I16 => Self::build::<i16>(&device, &config, sample_rate, Arc::clone(&tx), Arc::clone(&stopped))?,
            cpal::SampleFormat::F32 => Self::build::<f32>(&device, &config, sample_rate, Arc::clone(&tx), Arc::clone(&stopped))?,
            cpal::SampleFormat::U16 => Self::build::<u16>(&device, &config, sample_rate, Arc::clone(&tx), Arc::clone(&stopped))?,
            _ => unreachable!(),
        };

        Ok(Self { stream, stopped, tx })
    }

    fn supports_sample_rate(device: &cpal::Device, rate: u32) -> bool {
        if let Ok(configs) = device.supported_input_configs() {
            for config in configs {
                if config.min_sample_rate().0 <= rate && rate <= config.max_sample_rate().0 {
                    return true;
                }
            }
        }
        false
    }

    fn build<T>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        sample_rate: u32,
        tx: Arc<Mutex<Option<UnboundedSender<PcmCaptureMessage>>>>,
        stopped: Arc<AtomicBool>,
    ) -> Result<cpal::Stream>
    where
        T: cpal::Sample + cpal::SizedSample + Send + 'static,
        i16: FromSample<T>,
    {
        let err_fn = |e| error!("capture stream error: {e}");
        info!("building input stream with config: {:?}", config);
        let chunk_counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let chunk_counter_clone = std::sync::Arc::clone(&chunk_counter);
        let stream = device.build_input_stream(
            config,
            move |data: &[T], _| {
                if stopped.load(Ordering::Relaxed) {
                    return;
                }
                let pcm: Vec<i16> = data.iter().map(|s| i16::from_sample(*s)).collect();

                // Log audio levels periodically
                let count = chunk_counter_clone.fetch_add(1, Ordering::Relaxed);
                if count % 500 == 0 {
                    let max_level = pcm.iter().map(|s| s.abs()).max().unwrap_or(0);
                    info!("audio level: max={} (of 32767)", max_level);
                }

                // Resample to 24kHz
                let pcm_24k = match sample_rate {
                    16000 => resample_16_to_24(&pcm),
                    24000 => pcm,
                    48000 => resample_48_to_24(&pcm),
                    _ => pcm, // Unknown rate, pass through
                };

                if let Ok(guard) = tx.lock() {
                    if let Some(ref sender) = *guard {
                        let _ = sender.send(PcmCaptureMessage::Chunk(pcm_24k));
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

/// Downsample 48kHz to 24kHz by taking every 2nd sample
fn resample_48_to_24(samples: &[i16]) -> Vec<i16> {
    samples.iter().step_by(2).copied().collect()
}

/// Upsample 16kHz to 24kHz using linear interpolation
/// Ratio is 2:3 (for every 2 input samples, produce 3 output samples)
fn resample_16_to_24(samples: &[i16]) -> Vec<i16> {
    if samples.len() < 2 {
        return samples.to_vec();
    }

    // Output length = input_length * 3 / 2
    let output_len = (samples.len() * 3) / 2;
    let mut output = Vec::with_capacity(output_len);

    for out_idx in 0..output_len {
        // Position in input sample space:
        // out_idx at 24kHz corresponds to in_pos at 16kHz
        // in_pos = out_idx * 16000 / 24000 = out_idx * 2 / 3
        let in_pos_numer = out_idx * 2;
        let in_idx = in_pos_numer / 3;
        let frac = in_pos_numer % 3; // 0, 1, or 2 (representing 0/3, 1/3, 2/3)

        if in_idx >= samples.len() - 1 {
            output.push(samples[samples.len() - 1]);
        } else if frac == 0 {
            // Exact position, just copy
            output.push(samples[in_idx]);
        } else {
            // Linear interpolation between samples[in_idx] and samples[in_idx + 1]
            let s0 = samples[in_idx] as i32;
            let s1 = samples[in_idx + 1] as i32;
            // interp = s0 * (1 - frac/3) + s1 * (frac/3)
            //        = (s0 * (3 - frac) + s1 * frac) / 3
            let interp = (s0 * (3 - frac as i32) + s1 * frac as i32) / 3;
            output.push(interp as i16);
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // Tests for resample_48_to_24
    // ========================================================================

    #[test]
    fn test_resample_48_to_24_empty() {
        let input: Vec<i16> = vec![];
        let output = resample_48_to_24(&input);
        assert!(output.is_empty());
    }

    #[test]
    fn test_resample_48_to_24_single() {
        let input = vec![100];
        let output = resample_48_to_24(&input);
        assert_eq!(output, vec![100]);
    }

    #[test]
    fn test_resample_48_to_24_basic() {
        // Takes every 2nd sample
        let input = vec![0, 1, 2, 3, 4, 5, 6, 7];
        let output = resample_48_to_24(&input);
        assert_eq!(output, vec![0, 2, 4, 6]);
    }

    #[test]
    fn test_resample_48_to_24_output_length() {
        // Output should be half the input length
        for len in [2, 4, 6, 8, 10, 100, 1000] {
            let input: Vec<i16> = (0..len).map(|i| i as i16).collect();
            let output = resample_48_to_24(&input);
            assert_eq!(output.len(), len / 2, "input len={}", len);
        }
    }

    // ========================================================================
    // Tests for resample_16_to_24
    // ========================================================================

    #[test]
    fn test_resample_16_to_24_empty() {
        let input: Vec<i16> = vec![];
        let output = resample_16_to_24(&input);
        assert!(output.is_empty());
    }

    #[test]
    fn test_resample_16_to_24_single() {
        let input = vec![100];
        let output = resample_16_to_24(&input);
        assert_eq!(output, vec![100]);
    }

    #[test]
    fn test_resample_16_to_24_two_samples() {
        // 2 input samples → 3 output samples
        // Input: [0, 300] at positions 0, 1
        // Output positions (in input space): 0, 2/3, 4/3
        // out[0] = in[0] = 0
        // out[1] = in[0] * 1/3 + in[1] * 2/3 = 0 * 1/3 + 300 * 2/3 = 200
        // out[2] = in[1] * 2/3 + in[2] * 1/3, but in[2] doesn't exist, so use in[1] = 300
        let input = vec![0, 300];
        let output = resample_16_to_24(&input);
        assert_eq!(output.len(), 3);
        assert_eq!(output[0], 0);
        assert_eq!(output[1], 200); // (0 * 1 + 300 * 2) / 3 = 200
        assert_eq!(output[2], 300); // last sample
    }

    #[test]
    fn test_resample_16_to_24_four_samples() {
        // 4 input samples → 6 output samples
        // Input: [0, 300, 600, 900]
        // Output positions (in input space): 0, 2/3, 4/3, 2, 8/3, 10/3
        let input = vec![0, 300, 600, 900];
        let output = resample_16_to_24(&input);
        assert_eq!(output.len(), 6);

        // out[0] at 0: in[0] = 0
        assert_eq!(output[0], 0);

        // out[1] at 2/3: between in[0] and in[1], frac=2
        // = (0 * 1 + 300 * 2) / 3 = 200
        assert_eq!(output[1], 200);

        // out[2] at 4/3: between in[1] and in[2], frac=1
        // = (300 * 2 + 600 * 1) / 3 = 400
        assert_eq!(output[2], 400);

        // out[3] at 2: in[2] = 600
        assert_eq!(output[3], 600);

        // out[4] at 8/3: between in[2] and in[3], frac=2
        // = (600 * 1 + 900 * 2) / 3 = 800
        assert_eq!(output[4], 800);

        // out[5] at 10/3: between in[3] and in[4], but in[4] doesn't exist
        // = in[3] = 900
        assert_eq!(output[5], 900);
    }

    #[test]
    fn test_resample_16_to_24_output_length() {
        // Output should be 1.5x input length (input * 3 / 2)
        for len in [2, 4, 6, 8, 10, 100, 320, 1000] {
            let input: Vec<i16> = (0..len).map(|i| i as i16).collect();
            let output = resample_16_to_24(&input);
            let expected_len = (len * 3) / 2;
            assert_eq!(output.len(), expected_len, "input len={}", len);
        }
    }

    #[test]
    fn test_resample_16_to_24_constant_signal() {
        // A constant signal should remain constant after resampling
        let input: Vec<i16> = vec![1000; 100];
        let output = resample_16_to_24(&input);
        assert_eq!(output.len(), 150);
        for sample in output {
            assert_eq!(sample, 1000);
        }
    }

    #[test]
    fn test_resample_16_to_24_realistic_chunk() {
        // Test with a realistic chunk size (320 samples from 16kHz = 20ms)
        let input: Vec<i16> = (0..320).map(|i| (i * 100) as i16).collect();
        let output = resample_16_to_24(&input);
        // 320 * 3 / 2 = 480
        assert_eq!(output.len(), 480);

        // First sample should be unchanged
        assert_eq!(output[0], 0);

        // Values should be monotonically increasing (since input is)
        for i in 1..output.len() {
            assert!(output[i] >= output[i - 1], "output should be monotonic at {}", i);
        }
    }

    #[test]
    fn test_resample_roundtrip_length() {
        // If we resample 16k→24k→48k or similar, check lengths are as expected
        let input_16k: Vec<i16> = vec![0; 320]; // 20ms at 16kHz
        let output_24k = resample_16_to_24(&input_16k);
        assert_eq!(output_24k.len(), 480); // 20ms at 24kHz

        let input_48k: Vec<i16> = vec![0; 960]; // 20ms at 48kHz
        let output_24k = resample_48_to_24(&input_48k);
        assert_eq!(output_24k.len(), 480); // 20ms at 24kHz
    }
}
