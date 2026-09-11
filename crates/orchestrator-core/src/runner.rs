//! The runner: turns a loaded `Profile` list plus the three trait-crate
//! backends into actual running toggle/repeat/macro behavior. See
//! `docs/superpowers/specs/2026-08-14-runner-design.md` for the full design.

use crate::action::Jitter;

/// `Jitter::None` -> `interval_ms` fixed. `Jitter::Uniform { range_ms }` ->
/// `interval_ms +/- uniform(0, range_ms)`, symmetric around the interval.
/// `orchestrator-core`'s `Config::validate()` already requires
/// `range_ms <= interval_ms`, which is exactly what keeps this delay from
/// ever going negative (design spec §5.1).
pub(crate) fn compute_delay(interval_ms: u64, jitter: &Jitter) -> std::time::Duration {
    let millis = match jitter {
        Jitter::None => interval_ms,
        Jitter::Uniform { range_ms } if *range_ms == 0 => interval_ms,
        Jitter::Uniform { range_ms } => {
            let offset = fastrand::i64(-(*range_ms as i64)..=*range_ms as i64);
            (interval_ms as i64 + offset).max(0) as u64
        }
    };
    std::time::Duration::from_millis(millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_delay_none_jitter_is_exact_interval() {
        let d = compute_delay(100, &Jitter::None);
        assert_eq!(d, std::time::Duration::from_millis(100));
    }

    #[test]
    fn compute_delay_uniform_jitter_stays_within_symmetric_bounds() {
        let interval_ms = 100u64;
        let range_ms = 30u64;
        for _ in 0..500 {
            let d = compute_delay(interval_ms, &Jitter::Uniform { range_ms });
            let millis = d.as_millis() as i64;
            assert!(
                (interval_ms as i64 - range_ms as i64..=interval_ms as i64 + range_ms as i64)
                    .contains(&millis),
                "delay {millis}ms out of [{}, {}] bounds",
                interval_ms as i64 - range_ms as i64,
                interval_ms as i64 + range_ms as i64
            );
        }
    }

    #[test]
    fn compute_delay_zero_range_jitter_is_exact_interval() {
        let d = compute_delay(100, &Jitter::Uniform { range_ms: 0 });
        assert_eq!(d, std::time::Duration::from_millis(100));
    }
}
