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

/// Convert a chunk of 48 kHz linear PCM (i16) -> 8 kHz µ-law bytes.
/// Downsample 48k -> 8k and then encode µ-law.
/// Note: Gradium TTS outputs at 48kHz, not 24kHz!
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
