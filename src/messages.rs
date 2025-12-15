pub enum AudioCaptureMessage {
    Chunk(Vec<i16>),
}

pub enum AudioPlaybackMessage {
    Play(Vec<i16>),
    Reset,
}