//! Drain/guard state machine that closes a DeepSeek web SSE stream after `response/status=FINISHED`.
//! Ported from deepseek-web-done-terminator.js. Upstreams that leave the body open would otherwise
//! hang OpenAI SDK clients waiting for `data: [DONE]`.

use std::time::Duration;

pub(crate) const DEEPSEEK_FINISHED_DRAIN: Duration = Duration::from_millis(750);

/// Runs a finish callback at most once.
#[derive(Debug, Default)]
pub(crate) struct FinishOnce {
    finished: bool,
}

impl FinishOnce {
    /// Returns true only on the first call. Callers emit the terminal frames when it returns true.
    pub(crate) fn try_finish(&mut self) -> bool {
        if self.finished {
            return false;
        }
        self.finished = true;
        true
    }

    pub(crate) fn has_finished(&self) -> bool {
        self.finished
    }
}

/// Tracks the post-FINISHED drain window. The caller owns the timer. This type only answers
/// "should I still wait" and "reset the window" so it stays runtime-agnostic.
#[derive(Debug)]
pub(crate) struct DrainWindow {
    deadline: Option<std::time::Instant>,
    drain: Duration,
}

impl Default for DrainWindow {
    fn default() -> Self {
        Self::new(DEEPSEEK_FINISHED_DRAIN)
    }
}

impl DrainWindow {
    pub(crate) fn new(drain: Duration) -> Self {
        Self { deadline: None, drain }
    }

    /// Starts or restarts the window, as the JS scheduler does on each FINISHED or late payload.
    pub(crate) fn schedule(&mut self, now: std::time::Instant) {
        self.deadline = Some(now + self.drain);
    }

    pub(crate) fn clear(&mut self) {
        self.deadline = None;
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.deadline.is_some()
    }

    /// True once the window elapsed, meaning the stream must close now.
    pub(crate) fn is_due(&self, now: std::time::Instant) -> bool {
        self.deadline.is_some_and(|d| now >= d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn finish_runs_once() {
        let mut guard = FinishOnce::default();
        assert!(guard.try_finish());
        assert!(!guard.try_finish());
        assert!(guard.has_finished());
    }

    #[test]
    fn drain_reschedules_and_fires_after_window() {
        let mut window = DrainWindow::new(Duration::from_millis(50));
        let start = Instant::now();
        assert!(!window.is_pending());
        window.schedule(start);
        assert!(window.is_pending());
        assert!(!window.is_due(start + Duration::from_millis(10)));
        // A late payload restarts the window, so 60ms in is still before the new deadline.
        window.schedule(start + Duration::from_millis(40));
        assert!(!window.is_due(start + Duration::from_millis(60)));
        assert!(window.is_due(start + Duration::from_millis(91)));
        window.clear();
        assert!(!window.is_pending());
    }
}
