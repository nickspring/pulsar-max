use std::time::Duration;

const FIRST: Duration = Duration::from_secs(1);
const MAX: Duration = Duration::from_secs(30);
const STABLE_RUN: Duration = Duration::from_secs(60);

/// Restart delays for a helper process; a run that lasted resets the schedule.
#[derive(Debug, Default)]
pub struct Backoff {
    failures: u32,
}

impl Backoff {
    pub fn next_delay(&mut self, ran_for: Duration) -> Duration {
        if ran_for >= STABLE_RUN {
            self.failures = 0;
        }
        let delay = FIRST.saturating_mul(1u32 << self.failures.min(5)).min(MAX);
        self.failures = self.failures.saturating_add(1);
        delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn doubles_up_to_the_cap_while_runs_are_short() {
        let mut b = Backoff::default();
        let delays: Vec<u64> = (0..8).map(|_| b.next_delay(secs(1)).as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30, 30]);
    }

    #[test]
    fn a_long_run_starts_over() {
        let mut b = Backoff::default();
        for _ in 0..4 {
            b.next_delay(secs(1));
        }
        assert_eq!(b.next_delay(secs(60)), secs(1));
        assert_eq!(b.next_delay(secs(59)), secs(2));
    }
}
