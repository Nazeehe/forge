//! Shared load-robust timing assertions for perf tests.
//!
//! Wall-clock budgets flake while several agents share this box, so
//! perf tests compare two input sizes measured in the SAME run (load
//! cancels out) instead of asserting absolute milliseconds. Test-only:
//! the module is compiled out of non-test builds.

use std::time::{Duration, Instant};

/// A single scaling probe may never take this long; a breach means
/// catastrophe (hang, accidental superlinear blowup under no load),
/// not a missed budget.
pub(crate) const CATASTROPHE: Duration = Duration::from_secs(2);

/// Linear scaling is 4x; 10x leaves room for scheduler noise while a
/// quadratic 16x regression still fails loudly.
pub(crate) const MAX_RATIO: f64 = 10.0;

/// Minimum small-fixture time for the ratio to mean anything: below
/// scheduler jitter the denominator is noise. Callers size fixtures
/// so the small sample clears this on a fast box.
pub(crate) const MIN_SMALL: Duration = Duration::from_millis(5);

/// Rounds of alternating small/big measurement; the minimum of each
/// series wins, so one-sided stalls never set the verdict.
pub(crate) const ROUNDS: usize = 5;

/// Assert `big_work` costs ~4x `small_work` (linear scaling) with
/// both measured interleaved in the same run. Panics report both
/// durations; the ratio, not the milliseconds, is the verdict.
pub(crate) fn assert_scales_linearly(label: &str, small_work: impl Fn(), big_work: impl Fn()) {
    let mut t_small = Duration::MAX;
    let mut t_big = Duration::MAX;
    for _ in 0..ROUNDS {
        let start = Instant::now();
        small_work();
        t_small = t_small.min(start.elapsed());
        let start = Instant::now();
        big_work();
        t_big = t_big.min(start.elapsed());
    }
    assert!(
        t_small >= MIN_SMALL,
        "{label}: small sample {t_small:?} is scheduler noise, grow both fixtures"
    );
    assert!(
        t_big < CATASTROPHE,
        "{label}: catastrophe guard: big sample took {t_big:?}"
    );
    let ratio = t_big.as_secs_f64() / t_small.as_secs_f64();
    assert!(
        ratio <= MAX_RATIO,
        "{label}: superlinear scaling: big {t_big:?} vs small {t_small:?} (ratio {ratio:.1})"
    );
}
