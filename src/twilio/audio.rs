use audio_codec_algorithms::{decode_ulaw, encode_ulaw};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

/// Quality preset used for both up/down sampling.
fn sinc_params() -> SincInterpolationParameters {
    SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Cubic,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    }
}

/// Twilio frame size: 20ms at 8kHz = 160 samples = 160 bytes (µ-law)
pub const TWILIO_FRAME_BYTES: usize = 160;

/// Audio resampler with cached upsampler for streaming audio from Twilio to voice-agent.
/// Maintains internal state for proper sample-accurate streaming.
pub struct AudioResampler {
    /// Upsampler: 8kHz -> 24kHz (Twilio to voice-agent)
    upsampler: SincFixedIn<f32>,
}

impl AudioResampler {
    /// Create a new audio resampler with cached upsampler.
    /// chunk_size: typical chunk size for the upsampler (e.g., 160 for Twilio 20ms frames)
    pub fn new(chunk_size: usize) -> Result<Self, Box<dyn std::error::Error>> {
        // Upsampler: 8kHz -> 24kHz (3x ratio)
        let upsample_ratio = 24000.0 / 8000.0;
        let upsampler = SincFixedIn::<f32>::new(
            upsample_ratio,
            1.0, // max_rel
            sinc_params(),
            chunk_size,
            1, // mono
        )?;

        Ok(Self { upsampler })
    }

    /// Convert 8kHz µ-law bytes -> 24kHz PCM (i16).
    /// Maintains internal state for continuous streaming.
    pub fn ulaw8k_to_pcm24k(&mut self, ulaw_chunk: &[u8]) -> Vec<i16> {
        if ulaw_chunk.is_empty() {
            return Vec::new();
        }

        // 1) µ-law -> i16 @ 8kHz
        let pcm16_8k: Vec<i16> = ulaw_chunk.iter().map(|&b| decode_ulaw(b)).collect();

        // 2) i16 -> f32 [-1,1]
        let input_f32: Vec<f32> = pcm16_8k
            .into_iter()
            .map(|s| s as f32 / i16::MAX as f32)
            .collect();

        // 3) Resample 8kHz -> 24kHz
        let out_f32 = match self.upsampler.process(&[input_f32], None) {
            Ok(mut out) => out.remove(0),
            Err(e) => {
                eprintln!("Resample error: {}", e);
                return Vec::new();
            }
        };

        // 4) f32 -> i16
        out_f32
            .into_iter()
            .map(|x| {
                let v = (x * i16::MAX as f32).round();
                v.clamp(i16::MIN as f32, i16::MAX as f32) as i16
            })
            .collect()
    }
}

/// Stateful downsampler for TTS audio (48kHz PCM -> 8kHz µ-law).
/// Maintains internal buffer and resampler state for smooth streaming.
pub struct TtsDownsampler {
    /// Downsampler: 48kHz -> 8kHz
    downsampler: SincFixedIn<f32>,
    /// Buffer for incomplete output frames
    output_buffer: Vec<u8>,
}

impl TtsDownsampler {
    /// TTS chunk size at 48kHz (typical TTS output is ~80ms = 3840 samples)
    const TTS_CHUNK_SIZE: usize = 3840;

    /// Create a new TTS downsampler
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        // Downsampler: 48kHz -> 8kHz (1:6 ratio)
        let downsample_ratio = 8000.0 / 48000.0;
        let downsampler = SincFixedIn::<f32>::new(
            downsample_ratio,
            1.0, // max_rel
            sinc_params(),
            Self::TTS_CHUNK_SIZE,
            1, // mono
        )?;

        Ok(Self {
            downsampler,
            output_buffer: Vec::new(),
        })
    }

    /// Process 48kHz PCM samples and return complete 20ms µ-law frames for Twilio.
    /// Buffers incomplete frames internally.
    pub fn process(&mut self, pcm48_samples: &[i16]) -> Vec<Vec<u8>> {
        if pcm48_samples.is_empty() {
            return Vec::new();
        }

        // 1) i16 -> f32 [-1,1]
        let input_f32: Vec<f32> = pcm48_samples
            .iter()
            .map(|&s| s as f32 / i16::MAX as f32)
            .collect();

        // 2) Resample 48kHz -> 8kHz
        let out_f32 = match self.downsampler.process(&[input_f32], None) {
            Ok(mut out) => out.remove(0),
            Err(e) => {
                eprintln!("Downsample error: {}", e);
                return Vec::new();
            }
        };

        // 3) f32 -> i16 -> µ-law, add to buffer
        for sample in out_f32 {
            let v = (sample * i16::MAX as f32).round();
            let pcm16 = v.clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            self.output_buffer.push(encode_ulaw(pcm16));
        }

        // 4) Extract complete 20ms frames (160 bytes each)
        let mut frames = Vec::new();
        while self.output_buffer.len() >= TWILIO_FRAME_BYTES {
            let frame: Vec<u8> = self.output_buffer.drain(..TWILIO_FRAME_BYTES).collect();
            frames.push(frame);
        }

        frames
    }

    /// Flush any remaining buffered audio (for end of stream).
    /// Pads with silence if needed to complete the last frame.
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        if self.output_buffer.is_empty() {
            return None;
        }

        // Pad with silence (µ-law silence = 0xFF)
        while self.output_buffer.len() < TWILIO_FRAME_BYTES {
            self.output_buffer.push(0xFF);
        }

        Some(self.output_buffer.drain(..).collect())
    }
}

/// Simple one-shot conversion for small chunks (legacy compatibility).
/// Prefer TtsDownsampler for streaming to avoid discontinuities.
pub fn pcm48k_to_ulaw8k(pcm48_chunk: &[i16]) -> Vec<u8> {
    if pcm48_chunk.is_empty() {
        return Vec::new();
    }

    // 1) i16 -> f32 [-1,1]
    let input_f32: Vec<f32> = pcm48_chunk
        .iter()
        .map(|&s| s as f32 / i16::MAX as f32)
        .collect();

    // 2) 48k -> 8k (6:1 ratio)
    let ratio = 8000.0 / 48000.0;
    let params = sinc_params();
    let chunk_size = input_f32.len().max(1);
    let max_rel = 1.0; // fixed ratio
    let mut resampler =
        SincFixedIn::<f32>::new(ratio, max_rel, params, chunk_size, 1).expect("rubato init");

    let mut out = resampler.process(&[input_f32], None).expect("resample");
    let out_f32 = out.remove(0);

    // 3) f32 -> i16
    let pcm16_8k: Vec<i16> = out_f32
        .into_iter()
        .map(|x| {
            let v = (x * i16::MAX as f32).round();
            v.clamp(i16::MIN as f32, i16::MAX as f32) as i16
        })
        .collect();

    // 4) i16 -> µ-law
    pcm16_8k.into_iter().map(encode_ulaw).collect()
}
