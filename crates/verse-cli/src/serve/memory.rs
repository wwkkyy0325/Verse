//! How much memory this machine has, and how much of it the pool may hold.
//!
//! The pool's budget used to be a constant — a quarter of the 8 GB floor this
//! project requires — which meant a machine with 31 GB got the same pool as one
//! with 8. **That was wrong in the direction that costs the most**: the budget
//! was roughly the size of one Qwen3 worker, so the model that benefits most
//! from a pool got two workers where the machine could hold seven.
//!
//! It is measured now. Reading memory needs FFI on Windows and macOS, and this
//! project forbids `unsafe` in its own code, so `sysinfo` does it — the same
//! arrangement as sherpa-onnx doing the FFI for recognition. `verse-core` keeps
//! its empty dependency list, because the sizing *rule* takes bytes and this is
//! what supplies them.
//!
//! **Total, not available.** Total is a property of the machine; available is a
//! property of this moment. Sizing a long-lived pool from what happened to be
//! running at startup would make the pool smaller because somebody else was
//! compiling, and the user would have no way to see why.

use verse_core::MEMORY_BUDGET_BYTES;

/// The share of the machine's memory the pool may hold.
///
/// A quarter. The rest is for the operating system, the window's webview, the
/// audio being decoded, and whatever else the person is running — which on a
/// desktop is usually a good deal.
const SHARE: u64 = 4;

/// Bytes in a gibibyte — what Windows shows a person, and therefore what a
/// note about this machine should say.
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// What the pool's budget is, and where the number came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    pub bytes: u64,
    /// `probed` when the machine answered, `assumed` when it did not.
    pub source: &'static str,
    /// A sentence a client can read, saying which of those it is and why.
    pub note: String,
}

/// The budget, from the machine where it will say.
pub fn budget() -> Budget {
    from_total(total_bytes())
}

/// The same, from a total that has already been read — so the decision is
/// testable without depending on how much memory the test machine happens to
/// have.
pub fn from_total(total: Option<u64>) -> Budget {
    match total {
        // Zero is sysinfo saying it does not know, which is not a machine with
        // no memory. Treated as "would not say" rather than divided.
        Some(total) if total > 0 => Budget {
            bytes: total / SHARE,
            source: "probed",
            // GiB, not GB, because that is what Windows shows a person in
            // Settings — reporting the same machine as "33 GB" here and
            // "31.2 GB" there would look like a disagreement.
            note: format!(
                "a quarter of the {:.1} GiB this machine reports",
                total as f64 / GIB
            ),
        },
        _ => Budget {
            bytes: MEMORY_BUDGET_BYTES,
            source: "assumed",
            note: format!(
                "assumed, not probed: a quarter of the {:.0} GiB machine floor design.md \
                 states. The machine did not report how much memory it has.",
                (MEMORY_BUDGET_BYTES * SHARE) as f64 / GIB
            ),
        },
    }
}

/// Total physical memory, in bytes.
///
/// `None` when the machine will not say. Deliberately not an error: a pool that
/// cannot learn how much memory it has should be conservative, not refuse to
/// start.
fn total_bytes() -> Option<u64> {
    // `new()` then `refresh_memory()`, not `new_all()`. Two reasons, and the
    // first is not performance:
    //
    // `new_all()` populates every process on the machine, which is a lot of
    // work to learn one number; and `new()` alone leaves the field at zero —
    // which is what this did first, and what the test below caught. Zero is
    // indistinguishable from a machine with no memory, and dividing it would
    // produce a budget of nothing rather than an obvious failure.
    let mut system = sysinfo::System::new();
    system.refresh_memory();

    let total = system.total_memory();
    (total > 0).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_that_answers_gets_a_quarter_of_what_it_says() {
        let budget = from_total(Some(32_000_000_000));

        assert_eq!(budget.bytes, 8_000_000_000);
        assert_eq!(budget.source, "probed");
        assert!(budget.note.contains("29.8"), "got: {}", budget.note);
    }

    #[test]
    fn a_machine_that_will_not_answer_falls_back_and_says_so() {
        // The fallback is the constant this used to be, and the note has to
        // admit it — a client reading `budgetBytes` deserves to know whether it
        // came from the machine or from a document.
        for total in [None, Some(0)] {
            let budget = from_total(total);

            assert_eq!(budget.bytes, MEMORY_BUDGET_BYTES);
            assert_eq!(budget.source, "assumed");
            assert!(
                budget.note.contains("assumed"),
                "the note must not claim a probe that did not happen: {}",
                budget.note
            );
        }
    }

    #[test]
    fn a_small_machine_gets_a_small_budget_rather_than_the_floor() {
        // The whole point of probing. Four gigabytes is below the floor this
        // project requires, and the pool should be sized for the machine it is
        // on rather than for the floor in the document.
        let budget = from_total(Some(4_000_000_000));
        assert_eq!(budget.bytes, 1_000_000_000);
        assert_eq!(budget.source, "probed");
    }

    #[test]
    fn this_machine_reports_its_memory() {
        // Not an assertion about the value — that would pin the test to one
        // machine. It asserts the probe *works here*, which is the difference
        // between a dependency that earns its place and one that silently
        // returns nothing and leaves the fallback doing the work.
        let total = total_bytes();
        assert!(
            total.is_some_and(|bytes| bytes > 1_000_000_000),
            "the probe returned {total:?}; if this fails the dependency is not \
             doing anything and should be removed"
        );
    }
}
