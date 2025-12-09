use std::time::Instant;
use tracing::{debug, info};

pub struct PauseDetector {
    inactivity_prob: f32,
    last_text_time: Option<Instant>,
    last_flush_time: Option<Instant>,
}

impl PauseDetector {
    const PAUSE_THRESHOLD_MS: u64 = 0;
    const FLUSH_THRESHOLD_MS: u64 = 400;
    const PAUSE_THRESHOLD_INACTIVITY_PROB: f32 = 0.7;

    pub fn new() -> Self {
        Self {
            inactivity_prob: 0.0,
            last_text_time: None,
            last_flush_time: None,
        }
    }

    pub fn add_inactivity_prob(&mut self, inactivity_prob: f32) {
        self.inactivity_prob = inactivity_prob;
    }

    pub fn is_in_high_inactivity(&self) -> bool {
        self.inactivity_prob > Self::PAUSE_THRESHOLD_INACTIVITY_PROB
    }

    pub fn add_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        self.last_text_time = Some(Instant::now());
    }

    pub fn on_flush(&mut self) {
        self.last_flush_time = Some(Instant::now());
    }

    pub fn is_paused(&mut self) -> bool {
        let Some(last_text_time) = self.last_text_time else {
            return false;
        };

        let Some(last_flush_time) = self.last_flush_time else {
            return false;
        };

        let elapsed_ms_since_last_text = last_text_time.elapsed().as_millis() as u64;
        let elapsed_ms_since_last_flush = last_flush_time.elapsed().as_millis() as u64;

        let result = elapsed_ms_since_last_text >= Self::PAUSE_THRESHOLD_MS
            && elapsed_ms_since_last_flush >= Self::FLUSH_THRESHOLD_MS
            && self.inactivity_prob > Self::PAUSE_THRESHOLD_INACTIVITY_PROB;
        if result {
            info!(elapsed_ms_since_last_text=elapsed_ms_since_last_text,
                elapsed_ms_since_last_flush=elapsed_ms_since_last_flush,
                inactivity_prob=self.inactivity_prob,
                "Pause detected");
        }
        result
    }

    pub fn reset(&mut self) {
        self.last_text_time = None;
        self.last_flush_time = None;
        self.inactivity_prob = 0.0;
    }
}
