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
    fn the_summary_mentions_every_field() {
        let summary = profile(true, 4).summary();
        assert!(summary.contains('4'));
        assert!(summary.contains("avx2"));
        assert!(summary.contains("fma"));
    }
}
