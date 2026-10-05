//! Download progress.
//!
//! Downloads are always user-initiated. There is no fetch on startup and no
//! background refresh — the state below begins at [`DownloadState::Idle`] and
//! only moves because something called [`Downloader::fetch`].
//!
//! [`Downloader::fetch`]: crate::Downloader::fetch

/// Where a download attempt has got to.
///
/// The transitions, in full:
///
/// ```text
///   Idle ──fetch()──▶ Fetching { file: a }
///                        │
///                        ├── a done, b remains ──▶ Fetching { file: b }
///                        │
///                        ├── a mirror fails ──▶ Fetching { mirror: next }
///                        │      (last mirror) ──▶ Failed
///                        │
///                        └── all files done ──▶ Verifying
///                                                   │
///                                                   ├── ok ──▶ Ready
///                                                   └── short ──▶ Failed
/// ```
///
/// `Failed` is terminal for one call but not for the downloader: calling
/// `fetch` again starts over from `Idle`, and files already complete are
/// skipped rather than refetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadState {
    /// Nothing in progress.
    Idle,

    /// Pulling one file from one mirror.
    Fetching {
        /// The mirror being tried, by name.
        mirror: String,
        /// The file being pulled.
        file: String,
        /// Bytes of this file already on disk.
        received: u64,
        /// Expected total for this file, when the server says.
        total: Option<u64>,
    },

    /// Everything is on disk; checking what arrived.
    Verifying,

    /// Complete and usable.
    Ready,

    /// Gave up. `reason` is written for the user, not for a log.
    Failed { reason: String },
}

impl DownloadState {
    /// Whether work is under way.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            DownloadState::Fetching { .. } | DownloadState::Verifying
        )
    }

    /// Whether this attempt has stopped, successfully or not.
    pub fn is_settled(&self) -> bool {
        matches!(self, DownloadState::Ready | DownloadState::Failed { .. })
    }

    /// Whether the model is usable.
    pub fn is_ready(&self) -> bool {
        matches!(self, DownloadState::Ready)
    }

    /// Progress through the file in flight, when its size is known.
    ///
    /// This is per-file, not per-model: the caller does not know how many
    /// files remain, and inventing an overall figure would show a jump every
    /// time one finishes.
    pub fn fraction(&self) -> Option<f32> {
        match self {
            DownloadState::Fetching {
                received,
                total: Some(total),
                ..
            } if *total > 0 => Some((*received as f32 / *total as f32).clamp(0.0, 1.0)),
            _ => None,
        }
    }

    /// The failure message, if this attempt failed.
    pub fn reason(&self) -> Option<&str> {
        match self {
            DownloadState::Failed { reason } => Some(reason),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetching(received: u64, total: Option<u64>) -> DownloadState {
        DownloadState::Fetching {
            mirror: "hf-mirror".to_string(),
            file: "model.int8.onnx".to_string(),
            received,
            total,
        }
    }

    #[test]
    fn only_fetching_and_verifying_are_active() {
        assert!(fetching(0, None).is_active());
        assert!(DownloadState::Verifying.is_active());

        assert!(!DownloadState::Idle.is_active());
        assert!(!DownloadState::Ready.is_active());
        assert!(!DownloadState::Failed { reason: "x".into() }.is_active());
    }

    #[test]
    fn settled_covers_both_outcomes() {
        assert!(DownloadState::Ready.is_settled());
        assert!(DownloadState::Failed { reason: "x".into() }.is_settled());

        assert!(!DownloadState::Idle.is_settled());
        assert!(!fetching(0, None).is_settled());
    }

    #[test]
    fn fraction_needs_a_known_total() {
        assert_eq!(fetching(50, Some(100)).fraction(), Some(0.5));
        assert_eq!(fetching(50, None).fraction(), None);
        // A zero total would divide by zero; it means "unknown", not "done".
        assert_eq!(fetching(0, Some(0)).fraction(), None);
    }

    #[test]
    fn fraction_never_leaves_the_unit_interval() {
        // Servers sometimes report fewer bytes than they send.
        assert_eq!(fetching(200, Some(100)).fraction(), Some(1.0));
    }

    #[test]
    fn only_failed_carries_a_reason() {
        let failed = DownloadState::Failed {
            reason: "no mirror".into(),
        };
        assert_eq!(failed.reason(), Some("no mirror"));
        assert_eq!(DownloadState::Ready.reason(), None);
    }
}
