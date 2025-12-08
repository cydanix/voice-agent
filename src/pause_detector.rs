use std::time::Instant;
use tracing::{debug, info};

pub struct PauseDetector {
    /// Threshold to enter high-inactivity state
    enter_threshold: f32,
    /// Threshold to exit high-inactivity state (hysteresis)
    exit_threshold: f32,
    /// Duration in ms before pause is detected
    pause_duration_ms: u64,
    /// Minimum time since last text before pause can be detected
    min_silence_after_text_ms: u64,
    /// Additional delay after pause conditions are first met, to allow pending text to arrive
    confirmation_delay_ms: u64,
    /// When we entered high-inactivity state
    high_inactivity_start: Option<Instant>,
    /// Whether we're currently in high-inactivity state
    in_high_inactivity: bool,
    /// Minimum inactivity probability during the window
    min_inactivity_prob: f32,
    /// Current inactivity probability
    current_inactivity_prob: f32,
    /// When we last received text
    last_text_time: Option<Instant>,
    /// When pause conditions were first met (for confirmation delay)
    pause_first_detected: Option<Instant>,
}

impl PauseDetector {
    pub fn new() -> Self {
        Self::with_config(0.6, 0.4, 200, 300, 100)
    }

    pub fn with_config(
        enter_threshold: f32,
        exit_threshold: f32,
        pause_duration_ms: u64,
        min_silence_after_text_ms: u64,
        confirmation_delay_ms: u64,
    ) -> Self {
        Self {
            enter_threshold,
            exit_threshold,
            pause_duration_ms,
            min_silence_after_text_ms,
            confirmation_delay_ms,
            high_inactivity_start: None,
            in_high_inactivity: false,
            min_inactivity_prob: 1.0,
            current_inactivity_prob: 0.0,
            last_text_time: None,
            pause_first_detected: None,
        }
    }

    pub fn add_inactivity_prob(&mut self, inactivity_prob: f32) {
        self.current_inactivity_prob = inactivity_prob;

        if self.in_high_inactivity {
            // Track minimum during the window
            self.min_inactivity_prob = self.min_inactivity_prob.min(inactivity_prob);

            // Exit high-inactivity state if below exit threshold (hysteresis)
            if inactivity_prob < self.exit_threshold {
                let elapsed_ms = self.high_inactivity_start
                    .map(|s| s.elapsed().as_millis())
                    .unwrap_or(0);
                info!(
                    prob = inactivity_prob,
                    exit_threshold = self.exit_threshold,
                    elapsed_ms = elapsed_ms,
                    min_prob = self.min_inactivity_prob,
                    "pause_detector: exiting high-inactivity state (prob dropped below exit threshold)"
                );
                self.in_high_inactivity = false;
                self.high_inactivity_start = None;
                self.min_inactivity_prob = 1.0;
            } else {
                let elapsed_ms = self.high_inactivity_start
                    .map(|s| s.elapsed().as_millis())
                    .unwrap_or(0);
                debug!(
                    prob = inactivity_prob,
                    min_prob = self.min_inactivity_prob,
                    elapsed_ms = elapsed_ms,
                    "pause_detector: in high-inactivity state"
                );
            }
        } else {
            // Enter high-inactivity state if above enter threshold
            if inactivity_prob > self.enter_threshold {
                info!(
                    prob = inactivity_prob,
                    enter_threshold = self.enter_threshold,
                    "pause_detector: entering high-inactivity state"
                );
                self.in_high_inactivity = true;
                self.high_inactivity_start = Some(Instant::now());
                self.min_inactivity_prob = inactivity_prob;
            }
        }
    }

    pub fn add_text(&mut self, text: &str) {
        if !text.is_empty() {
            if self.in_high_inactivity || self.pause_first_detected.is_some() {
                let elapsed_ms = self.high_inactivity_start
                    .map(|s| s.elapsed().as_millis())
                    .unwrap_or(0);
                info!(
                    text_len = text.len(),
                    elapsed_ms = elapsed_ms,
                    "pause_detector: text received, resetting state"
                );
            }
            // Track when we last received text
            self.last_text_time = Some(Instant::now());
            // Reset the timer - user is still speaking
            self.high_inactivity_start = None;
            self.in_high_inactivity = false;
            self.min_inactivity_prob = 1.0;
            // Reset pause confirmation - text arrived, so pause is cancelled
            self.pause_first_detected = None;
        }
    }

    pub fn is_paused(&mut self) -> bool {
        // Pause is detected when:
        // 1. We have a high inactivity start time
        // 2. pause_duration_ms has passed since then
        // 3. We're still in high-inactivity state
        // 4. Minimum probability during window stayed above exit threshold
        // 5. Enough time has passed since last text (to handle STT latency)
        // 6. Pause conditions have been stable for confirmation_delay_ms
        if let Some(start) = self.high_inactivity_start {
            let elapsed_ms = start.elapsed().as_millis();
            let has_duration_passed = elapsed_ms >= self.pause_duration_ms as u128;
            let min_stayed_high = self.min_inactivity_prob >= self.exit_threshold;

            // Check if enough time has passed since last text
            let silence_since_text = self.last_text_time
                .map(|t| t.elapsed().as_millis() >= self.min_silence_after_text_ms as u128)
                .unwrap_or(true); // If no text received yet, allow pause

            let basic_conditions_met = has_duration_passed && self.in_high_inactivity && min_stayed_high && silence_since_text;

            if basic_conditions_met {
                // Track when conditions were first met
                if self.pause_first_detected.is_none() {
                    self.pause_first_detected = Some(Instant::now());
                    debug!(
                        elapsed_ms = elapsed_ms,
                        "pause_detector: pause conditions first met, waiting for confirmation"
                    );
                }

                // Check if confirmation delay has passed
                let confirmed = self.pause_first_detected
                    .map(|t| t.elapsed().as_millis() >= self.confirmation_delay_ms as u128)
                    .unwrap_or(false);

                if confirmed {
                    let ms_since_text = self.last_text_time
                        .map(|t| t.elapsed().as_millis())
                        .unwrap_or(0);
                    let confirmation_ms = self.pause_first_detected
                        .map(|t| t.elapsed().as_millis())
                        .unwrap_or(0);

                    debug!(
                        elapsed_ms = elapsed_ms,
                        min_prob = self.min_inactivity_prob,
                        current_prob = self.current_inactivity_prob,
                        ms_since_text = ms_since_text,
                        confirmation_ms = confirmation_ms,
                        "pause_detector: PAUSE CONFIRMED"
                    );
                    return true;
                }
            } else {
                // Conditions no longer met, reset confirmation
                self.pause_first_detected = None;
            }

            false
        } else {
            self.pause_first_detected = None;
            false
        }
    }

    pub fn reset(&mut self) {
        debug!("pause_detector: reset called");
        self.high_inactivity_start = None;
        self.in_high_inactivity = false;
        self.min_inactivity_prob = 1.0;
        self.current_inactivity_prob = 0.0;
        self.pause_first_detected = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn test_initial_state_not_paused() {
        let mut detector = PauseDetector::new();
        assert!(!detector.is_paused());
    }

    #[test]
    fn test_high_inactivity_not_paused_immediately() {
        let mut detector = PauseDetector::new();
        detector.add_inactivity_prob(0.8);
        // Should not be paused immediately, need to wait 200ms
        assert!(!detector.is_paused());
    }

    #[test]
    fn test_high_inactivity_paused_after_200ms() {
        let mut detector = PauseDetector::new();
        detector.add_inactivity_prob(0.8);
        sleep(Duration::from_millis(250));
        // Still high inactivity, 200ms passed, no text received
        detector.add_inactivity_prob(0.7);
        // First call starts confirmation timer
        assert!(!detector.is_paused());
        sleep(Duration::from_millis(150)); // Wait for confirmation (100ms default)
        detector.add_inactivity_prob(0.7);
        assert!(detector.is_paused());
    }

    #[test]
    fn test_text_resets_timer() {
        let mut detector = PauseDetector::new();
        detector.add_inactivity_prob(0.8);
        sleep(Duration::from_millis(100));
        detector.add_text("hello");
        // Text resets the timer, so we need another 200ms
        sleep(Duration::from_millis(50));
        detector.add_inactivity_prob(0.8);
        // Only 50ms since text, should not be paused yet
        assert!(!detector.is_paused());
    }

    #[test]
    fn test_text_resets_then_pause_detected() {
        let mut detector = PauseDetector::new();
        detector.add_inactivity_prob(0.8);
        sleep(Duration::from_millis(100));
        detector.add_text("hello");
        // Text resets the timer
        detector.add_inactivity_prob(0.8);
        sleep(Duration::from_millis(450)); // Need 300ms silence + 200ms pause + 100ms confirm
        detector.add_inactivity_prob(0.7);
        assert!(!detector.is_paused()); // Starts confirmation
        sleep(Duration::from_millis(150));
        detector.add_inactivity_prob(0.7);
        assert!(detector.is_paused());
    }

    #[test]
    fn test_empty_text_does_not_reset() {
        let mut detector = PauseDetector::new();
        detector.add_inactivity_prob(0.8);
        sleep(Duration::from_millis(100));
        detector.add_text("");
        sleep(Duration::from_millis(150));
        detector.add_inactivity_prob(0.7);
        // Empty text should not reset timer, starts confirmation
        assert!(!detector.is_paused());
        sleep(Duration::from_millis(150));
        detector.add_inactivity_prob(0.7);
        assert!(detector.is_paused());
    }

    #[test]
    fn test_hysteresis_stays_high_in_middle_zone() {
        let mut detector = PauseDetector::new();
        // Enter high state (prob > 0.6)
        detector.add_inactivity_prob(0.8);
        sleep(Duration::from_millis(100));
        // Drop to middle zone (0.4 < prob < 0.6) - should stay in high state
        detector.add_inactivity_prob(0.5);
        sleep(Duration::from_millis(150));
        detector.add_inactivity_prob(0.5);
        // Starts confirmation
        assert!(!detector.is_paused());
        sleep(Duration::from_millis(150));
        detector.add_inactivity_prob(0.5);
        // Should be paused because we didn't drop below exit threshold (0.4)
        assert!(detector.is_paused());
    }

    #[test]
    fn test_hysteresis_exits_below_exit_threshold() {
        let mut detector = PauseDetector::new();
        detector.add_inactivity_prob(0.8);
        sleep(Duration::from_millis(150));
        // Drop below exit threshold (0.4)
        detector.add_inactivity_prob(0.3);
        sleep(Duration::from_millis(100));
        // Goes high again
        detector.add_inactivity_prob(0.8);
        // Should not be paused yet, timer was reset
        assert!(!detector.is_paused());
    }

    #[test]
    fn test_does_not_enter_below_enter_threshold() {
        let mut detector = PauseDetector::new();
        // 0.5 is below enter threshold (0.6)
        detector.add_inactivity_prob(0.5);
        sleep(Duration::from_millis(250));
        detector.add_inactivity_prob(0.5);
        assert!(!detector.is_paused());
    }

    #[test]
    fn test_reset_clears_state() {
        let mut detector = PauseDetector::new();
        detector.add_inactivity_prob(0.8);
        sleep(Duration::from_millis(250));
        detector.add_inactivity_prob(0.7);
        assert!(!detector.is_paused()); // Starts confirmation
        sleep(Duration::from_millis(150));
        detector.add_inactivity_prob(0.7);
        assert!(detector.is_paused());

        detector.reset();
        assert!(!detector.is_paused());
    }

    #[test]
    fn test_inactivity_drops_after_duration() {
        let mut detector = PauseDetector::new();
        detector.add_inactivity_prob(0.8);
        sleep(Duration::from_millis(250));
        // Inactivity drops below exit threshold after 200ms
        detector.add_inactivity_prob(0.3);
        // Should not be paused because we exited high-inactivity state
        assert!(!detector.is_paused());
    }

    #[test]
    fn test_text_before_high_inactivity_does_not_matter() {
        let mut detector = PauseDetector::new();
        detector.add_text("hello");
        detector.add_inactivity_prob(0.8);
        sleep(Duration::from_millis(450)); // Need 300ms silence + 200ms pause
        detector.add_inactivity_prob(0.7);
        // Starts confirmation
        assert!(!detector.is_paused());
        sleep(Duration::from_millis(150));
        detector.add_inactivity_prob(0.7);
        // Text was before high inactivity started, should pause after confirmation
        assert!(detector.is_paused());
    }

    #[test]
    fn test_min_prob_dip_prevents_pause() {
        let mut detector = PauseDetector::new();
        detector.add_inactivity_prob(0.8);
        sleep(Duration::from_millis(100));
        // Brief dip below exit threshold (0.4)
        detector.add_inactivity_prob(0.35);
        sleep(Duration::from_millis(150));
        // This exits the high state, so new prob won't immediately trigger pause
        detector.add_inactivity_prob(0.8);
        assert!(!detector.is_paused());
    }

    #[test]
    fn test_min_prob_stays_above_exit_threshold() {
        let mut detector = PauseDetector::new();
        detector.add_inactivity_prob(0.8);
        sleep(Duration::from_millis(100));
        // Dip but stay above exit threshold (0.4)
        detector.add_inactivity_prob(0.45);
        sleep(Duration::from_millis(150));
        detector.add_inactivity_prob(0.7);
        // Starts confirmation
        assert!(!detector.is_paused());
        sleep(Duration::from_millis(150));
        detector.add_inactivity_prob(0.7);
        // Min stayed above exit threshold, should be paused
        assert!(detector.is_paused());
    }

    #[test]
    fn test_custom_config() {
        // Custom config: enter at 0.7, exit at 0.3, 100ms duration, 50ms silence, 50ms confirm
        let mut detector = PauseDetector::with_config(0.7, 0.3, 100, 50, 50);

        // 0.65 is below enter threshold (0.7)
        detector.add_inactivity_prob(0.65);
        sleep(Duration::from_millis(200));
        detector.add_inactivity_prob(0.65);
        assert!(!detector.is_paused());

        // 0.75 is above enter threshold
        detector.add_inactivity_prob(0.75);
        sleep(Duration::from_millis(150)); // 150ms > 100ms pause duration
        detector.add_inactivity_prob(0.75);
        assert!(!detector.is_paused()); // Starts confirmation
        sleep(Duration::from_millis(100)); // Wait for 50ms confirmation
        detector.add_inactivity_prob(0.75);
        assert!(detector.is_paused());
    }

    #[test]
    fn test_min_silence_after_text_prevents_early_pause() {
        // Use short pause duration, longer silence requirement, short confirmation
        let mut detector = PauseDetector::with_config(0.6, 0.4, 100, 300, 50);

        detector.add_inactivity_prob(0.8);
        sleep(Duration::from_millis(50));
        detector.add_text("hello");
        // Text received, now need 300ms silence

        detector.add_inactivity_prob(0.8);
        sleep(Duration::from_millis(150)); // 150ms since text
        detector.add_inactivity_prob(0.8);
        // 150ms since text, but need 300ms - should NOT be paused
        assert!(!detector.is_paused());

        sleep(Duration::from_millis(200)); // Now 350ms since text
        detector.add_inactivity_prob(0.8);
        // Starts confirmation
        assert!(!detector.is_paused());
        sleep(Duration::from_millis(100)); // Wait for 50ms confirmation
        detector.add_inactivity_prob(0.8);
        // Now enough silence has passed + confirmation, should be paused
        assert!(detector.is_paused());
    }

    #[test]
    fn test_late_text_prevents_pause() {
        // Simulates the real scenario: high inactivity detected, but text arrives during confirmation
        let mut detector = PauseDetector::with_config(0.6, 0.4, 200, 300, 100);

        detector.add_inactivity_prob(0.8);
        sleep(Duration::from_millis(250)); // 250ms high inactivity, conditions met
        detector.add_inactivity_prob(0.8);
        // is_paused() starts confirmation timer
        assert!(!detector.is_paused()); // Not confirmed yet (need 100ms)

        sleep(Duration::from_millis(50)); // 50ms into confirmation
        // Text arrives during confirmation window!
        detector.add_text("France?");
        detector.add_inactivity_prob(0.7);
        // Text cancelled the confirmation - should NOT be paused
        assert!(!detector.is_paused());
    }

    #[test]
    fn test_confirmation_delay() {
        // Test that pause is only confirmed after confirmation_delay_ms
        let mut detector = PauseDetector::with_config(0.6, 0.4, 100, 0, 100);

        detector.add_inactivity_prob(0.8);
        sleep(Duration::from_millis(150)); // 150ms high inactivity (> 100ms pause duration)
        detector.add_inactivity_prob(0.8);

        // First check - starts confirmation timer
        assert!(!detector.is_paused());

        sleep(Duration::from_millis(50)); // 50ms into confirmation (need 100ms)
        detector.add_inactivity_prob(0.8);
        assert!(!detector.is_paused()); // Still not confirmed

        sleep(Duration::from_millis(60)); // Now 110ms into confirmation
        detector.add_inactivity_prob(0.8);
        assert!(detector.is_paused()); // Now confirmed!
    }
}