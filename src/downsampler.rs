//! Simple 48kHz → 24kHz downsampler (factor of 2).

/// Downsample PCM from 48kHz to 24kHz by averaging pairs of samples.
pub fn downsample_48_to_24(samples: &[i16]) -> Vec<i16> {
    samples
        .chunks(2)
        .map(|pair| {
            if pair.len() == 2 {
                ((pair[0] as i32 + pair[1] as i32) / 2) as i16
            } else {
                pair[0]
            }
        })
        .collect()
}

