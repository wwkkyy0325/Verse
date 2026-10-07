//! What the machine can do, and what follows from it.
//!
//! Detection never fails and never blocks startup. A machine that cannot run
//! the big models runs the small ones and says why — refusing to start would
//! be the wrong answer to "this CPU is old".
//!
//! This module deliberately knows nothing about models. It reports capability;
//! deciding what that capability permits belongs to whoever is choosing a
//! model.

/// Detected capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardwareProfile {
    /// AVX2, which the recognition kernels use when present.
    pub avx2: bool,
    /// Fused multiply-add.
    pub fma: bool,
    /// Logical cores.
    pub cores: usize,
}

/// How much this machine should be asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Nothing to work around.
    Full,
    /// Run small models, and say why.
    Reduced { reason: &'static str },
}

impl Default for HardwareProfile {
    fn default() -> Self {
        Self::probe()
    }
}

impl HardwareProfile {
    /// Inspect the current machine.
    pub fn probe() -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);

        Self {
            avx2: has_avx2(),
            fma: has_fma(),
            cores,
        }
    }

    /// Threads to give a recognition engine.
    ///
    /// One core is held back. On an older machine that is the difference
    /// between a responsive window and a frozen one, and on a newer one it
    /// costs nothing worth measuring.
    pub fn engine_threads(&self) -> usize {
        self.cores.saturating_sub(1).max(1)
    }

    /// How many recognition workers this machine should run at once.
    ///
    /// `per_worker_bytes` is what one resident worker costs — its weights plus
    /// [`RUNTIME_OVERHEAD_BYTES`]. Taken as a number rather than as a model,
    /// because this crate knows nothing about models and does not want to.
    ///
    /// **The budget is an assumption, not a measurement.** Reading the machine's
    /// RAM needs FFI and `unsafe` on every platform this project targets, and
    /// the project has none. So the ceiling is [`MEMORY_BUDGET_BYTES`], reasoned
    /// from the 8 GB machine floor `design.md` §3 states, and callers are
    /// expected to report it rather than to trust it.
    ///
    /// Two bounds are not about memory. A worker needs at least one thread, so
    /// the pool can never exceed [`HardwareProfile::engine_threads`]; and
    /// [`MAX_WORKERS`] caps it however the budget reads, so the ceiling is a
    /// number someone can audit rather than a consequence of multiplication.
    pub fn pool_size(&self, per_worker_bytes: u64, budget_bytes: u64) -> usize {
        pool_size(self.engine_threads(), per_worker_bytes, budget_bytes)
    }

    /// Which tier this machine falls into.
    pub fn tier(&self) -> Tier {
        if !self.avx2 {
            // The recognition kernels have fallback paths, but they are slow
            // enough that a large model would not keep up.
            return Tier::Reduced {
                reason: "this CPU does not support AVX2",
            };
        }

        if self.cores < 2 {
            return Tier::Reduced {
                reason: "this machine reports a single CPU core",
            };
        }

        Tier::Full
    }

    /// A one-line summary, for logs and diagnostics.
    pub fn summary(&self) -> String {
        format!(
            "{} core(s), avx2: {}, fma: {}",
            self.cores, self.avx2, self.fma
        )
    }
}

impl Tier {
    /// Whether anything is being worked around.
    pub fn is_reduced(&self) -> bool {
        matches!(self, Tier::Reduced { .. })
    }

    /// Why, when something is.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Tier::Reduced { reason } => Some(reason),
            Tier::Full => None,
        }
    }

    /// A message worth showing the user, or `None` when nothing is wrong.
    ///
    /// Phrased as something that was done, not something that failed — the
    /// point is that the app still works.
    pub fn notice(&self) -> Option<String> {
        self.reason()
            .map(|reason| format!("Running in reduced mode because {reason}."))
    }
}

/// The most models this program will hold resident at once, whatever the
/// arithmetic says.
///
/// A ceiling so the bound is a number someone can read rather than a
/// consequence of multiplication. Eight is the largest pool measured, and the
/// curve was still improving there — 6.03 s for 24 jobs at one worker, 2.02 s
/// at eight — so this is a deliberate stop rather than a knee that was found.
pub const MAX_WORKERS: usize = 8;

/// How much memory the workers may hold between them.
///
/// **A written assumption, not a probe.** Reading the machine's RAM needs FFI
/// and `unsafe`, which this project does not have, so this is reasoned from the
/// 8 GB floor `design.md` §3 commits to: a quarter of it, which leaves the rest
/// for the operating system, the window's webview, and the audio being decoded.
///
/// It is reported to clients so it can be disagreed with rather than merely
/// obeyed.
pub const MEMORY_BUDGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// What one worker costs beyond its weights.
///
/// The model file is not the whole story: an initialised session, its scratch
/// buffers and the decoded audio all scale with a worker.
///
/// **Measured.** A pool of 1 held 361 MB and one of 8 held 2619 MB, so each
/// additional worker costs about 322 MB against the 239.5 MB of weights it
/// loads — an overhead of roughly 82 MB, which is this number rounded. See the
/// curve in `docs/llmwiki/tasks/adaptive-pool.md`.
pub const RUNTIME_OVERHEAD_BYTES: u64 = 80 * 1024 * 1024;

/// How many workers the machine and the budget between them allow.
///
/// Pure, and takes bytes rather than anything model-shaped, so it is testable
/// with integers and so this crate stays ignorant of the catalogue.
pub fn pool_size(engine_threads: usize, per_worker_bytes: u64, budget_bytes: u64) -> usize {
    // A worker whose cost is unknown is one worker. Guessing a number here
    // would be the silent failure this project keeps having to design against.
    if per_worker_bytes == 0 {
        return 1;
    }

    let affordable = (budget_bytes / per_worker_bytes).max(1) as usize;

    // At least one thread each, so the pool can never ask for more CPU than the
    // machine has been willing to give.
    affordable
        .min(engine_threads.max(1))
        .clamp(1, MAX_WORKERS.max(1))
}

/// How many threads each of `workers` gets from a machine's budget.
///
/// Integer division, floored at one, because a worker with no threads is not a
/// worker. Shared by the command line's `--jobs` and the service's pool so the
/// two cannot disagree about what a worker asks for.
pub fn per_worker_threads(engine_threads: usize, workers: usize) -> usize {
    (engine_threads / workers.max(1)).max(1)
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn has_avx2() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn has_fma() -> bool {
    std::arch::is_x86_feature_detected!("fma")
}

/// Non-x86 targets in scope (Apple Silicon, ARMv8) all have NEON, which the
/// kernels use, so they are not treated as reduced.
#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn has_avx2() -> bool {
    true
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn has_fma() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(avx2: bool, cores: usize) -> HardwareProfile {
        HardwareProfile {
            avx2,
            fma: avx2,
            cores,
        }
    }

    #[test]
    fn probing_this_machine_produces_something_usable() {
        let profile = HardwareProfile::probe();
        assert!(profile.cores >= 1, "a machine has at least one core");
        assert!(profile.engine_threads() >= 1);
    }

    #[test]
    fn a_capable_machine_is_not_reduced() {
        let tier = profile(true, 8).tier();
        assert_eq!(tier, Tier::Full);
        assert!(
            tier.notice().is_none(),
            "nothing to report when nothing is wrong"
        );
    }

    #[test]
    fn a_cpu_without_avx2_is_reduced_and_says_so() {
        let tier = profile(false, 8).tier();
        assert!(tier.is_reduced());
        let notice = tier.notice().expect("reduced mode should explain itself");
        assert!(notice.contains("AVX2"), "got: {notice}");
        // The message must not read as a failure.
        assert!(notice.contains("reduced mode"), "got: {notice}");
    }

    #[test]
    fn a_single_core_machine_is_reduced() {
        let tier = profile(true, 1).tier();
        assert!(tier.is_reduced());
        assert!(tier.notice().unwrap().contains("single CPU core"));
    }

    #[test]
    fn one_core_is_always_left_for_the_ui() {
        assert_eq!(profile(true, 1).engine_threads(), 1);
        assert_eq!(profile(true, 4).engine_threads(), 3);
        assert_eq!(profile(true, 16).engine_threads(), 15);
    }

    #[test]
    fn the_budget_binds_before_the_threads_do_on_a_big_machine() {
        // The shape that matters: a worker costs what it costs, so the memory
        // ceiling decides long before fifteen threads run out.
        let machine = profile(true, 16);

        // SenseVoice: 239.5 MB of weights plus 80 MiB of overhead is 323.4 MB,
        // and the budget affords six of those. The measured marginal cost per
        // worker was 322 MB, so the arithmetic matches what the machine did.
        let small = 239_500_000 + RUNTIME_OVERHEAD_BYTES;
        assert_eq!(machine.pool_size(small, MEMORY_BUDGET_BYTES), 6);

        // Qwen3-ASR: 987 MB, so about two fit and no more. That the gigabyte
        // model lands low is the intent, not a shortfall.
        let large = 987_000_000 + RUNTIME_OVERHEAD_BYTES;
        assert_eq!(machine.pool_size(large, MEMORY_BUDGET_BYTES), 2);
    }

    #[test]
    fn a_small_machine_runs_fewer_workers_than_its_budget_allows() {
        // The thread bound, which is the other half of the rule: every worker
        // needs at least one thread, so a four-core machine cannot run eight of
        // them however much memory there is.
        let machine = profile(true, 4);
        assert_eq!(machine.engine_threads(), 3);
        assert_eq!(machine.pool_size(1, u64::MAX), 3);
    }

    #[test]
    fn a_single_core_machine_still_gets_one_worker() {
        let machine = profile(true, 1);
        assert_eq!(machine.engine_threads(), 1);
        assert_eq!(machine.pool_size(1, u64::MAX), 1);
    }

    #[test]
    fn an_unknown_cost_is_one_worker_rather_than_a_guess() {
        // Zero bytes means "not known". Treating it as free would multiply
        // workers until something fell over, which is the silent failure this
        // project has already been caught by twice.
        assert_eq!(pool_size(16, 0, MEMORY_BUDGET_BYTES), 1);
    }

    #[test]
    fn a_worker_that_costs_more_than_the_budget_is_still_one() {
        // It cannot be improved, and refusing to run at all would be worse than
        // running heavy. One, and the caller reports the cost.
        assert_eq!(pool_size(16, MEMORY_BUDGET_BYTES * 4, MEMORY_BUDGET_BYTES), 1);
    }

    #[test]
    fn the_ceiling_holds_however_large_the_budget_is() {
        // The bound is a number someone can read, not a product.
        assert_eq!(pool_size(usize::MAX, 1, u64::MAX), MAX_WORKERS);
    }

    #[test]
    fn the_thread_budget_is_divided_and_never_reaches_zero() {
        assert_eq!(per_worker_threads(15, 1), 15);
        assert_eq!(per_worker_threads(15, 4), 3);
        assert_eq!(per_worker_threads(15, 16), 1);
        assert_eq!(per_worker_threads(1, 8), 1);
        // A worker count of zero is nonsense; it must not divide by zero.
        assert_eq!(per_worker_threads(15, 0), 15);
    }

    #[test]
    fn the_summary_mentions_every_field() {
        let summary = profile(true, 4).summary();
        assert!(summary.contains('4'));
        assert!(summary.contains("avx2"));
        assert!(summary.contains("fma"));
    }
}
