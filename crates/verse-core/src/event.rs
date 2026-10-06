use crate::domain::{JobId, ModelId, ModelState, Segment, Transcript, TranscriptDelta};
use crate::error::ErrorInfo;
use crate::hardware::HardwareProfile;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

/// What kind of work a job represents. Determines which pipeline handles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobKind {
    /// Decode a file and produce a full transcript.
    FileTranscribe,
    /// Capture live audio and emit subtitles as speech happens.
    LiveSubtitle,
    /// Translate already-recognized text.
    Translate,
}

/// Everything that flows between modules.
///
/// Modules never call each other directly. They publish here and subscribe
/// with a filter, which is what keeps the UI independent of any pipeline and
/// lets Phase 2's subtitle window reuse Phase 1's plumbing unchanged.
#[derive(Debug, Clone)]
pub enum Event {
    // ---- job lifecycle ----
    JobStarted {
        id: JobId,
        kind: JobKind,
    },
    JobProgress {
        id: JobId,
        position: Duration,
        fraction: f32,
    },
    JobFinished {
        id: JobId,
    },
    JobFailed {
        id: JobId,
        error: ErrorInfo,
    },
    JobCancelled {
        id: JobId,
    },

    // ---- pipeline output ----
    TranscriptDelta {
        job: JobId,
        delta: TranscriptDelta,
    },
    TranscriptSegment {
        job: JobId,
        segment: Segment,
    },
    TranscriptFinal {
        job: JobId,
        transcript: Transcript,
    },
    /// Segments already published for this job are void.
    ///
    /// The segmenter can be distrusted after the fact — it reports almost no
    /// speech in a file that plainly contains some — and by then its spans
    /// have already been recognised and shown. Everything published for this
    /// job so far is about to be replaced, and a listener that does not act on
    /// this will be left displaying the fragment that was discarded.
    TranscriptDiscarded {
        job: JobId,
    },

    // ---- environment ----
    HardwareProbed {
        profile: HardwareProfile,
    },
    ModelStateChanged {
        model: ModelId,
        state: ModelState,
    },
}

impl Event {
    /// The job this event belongs to, if any.
    pub fn job(&self) -> Option<JobId> {
        match self {
            Event::JobStarted { id, .. }
            | Event::JobProgress { id, .. }
            | Event::JobFinished { id }
            | Event::JobFailed { id, .. }
            | Event::JobCancelled { id } => Some(*id),
            Event::TranscriptDelta { job, .. }
            | Event::TranscriptSegment { job, .. }
            | Event::TranscriptFinal { job, .. }
            | Event::TranscriptDiscarded { job } => Some(*job),
            Event::HardwareProbed { .. } | Event::ModelStateChanged { .. } => None,
        }
    }
}

type Filter = Box<dyn Fn(&Event) -> bool + Send + Sync>;

struct Subscriber {
    id: u64,
    filter: Filter,
    tx: Sender<Arc<Event>>,
}

struct Inner {
    subscribers: Mutex<Vec<Subscriber>>,
    next_id: AtomicU64,
}

/// Fan-out event bus.
///
/// Publishing is synchronous — every matching subscriber's channel has the
/// event queued before `publish` returns — but channels are unbounded, so a
/// slow subscriber accumulates rather than blocking the publisher. Dropping a
/// [`Subscription`] unsubscribes; dead subscribers are also reaped lazily on
/// the next publish.
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<Inner>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                subscribers: Mutex::new(Vec::new()),
                next_id: AtomicU64::new(1),
            }),
        }
    }

    /// Subscribe with a predicate. The filter runs on the publisher's thread,
    /// so keep it cheap.
    pub fn subscribe(
        &self,
        filter: impl Fn(&Event) -> bool + Send + Sync + 'static,
    ) -> Subscription {
        let (tx, rx) = channel();
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner.subscribers.lock().unwrap().push(Subscriber {
            id,
            filter: Box::new(filter),
            tx,
        });
        Subscription {
            id,
            rx,
            bus: Arc::downgrade(&self.inner),
        }
    }

    pub fn subscribe_all(&self) -> Subscription {
        self.subscribe(|_| true)
    }

    pub fn publish(&self, event: Event) {
        let event = Arc::new(event);
        let mut subs = self.inner.subscribers.lock().unwrap();
        subs.retain(|sub| {
            if (sub.filter)(&event) {
                // A failed send means the receiver is gone; reap the subscriber.
                sub.tx.send(Arc::clone(&event)).is_ok()
            } else {
                true
            }
        });
    }

    pub fn subscriber_count(&self) -> usize {
        self.inner.subscribers.lock().unwrap().len()
    }
}

/// A live subscription. Dropping it unsubscribes.
pub struct Subscription {
    id: u64,
    rx: Receiver<Arc<Event>>,
    bus: Weak<Inner>,
}

impl Subscription {
    pub fn try_recv(&self) -> Option<Arc<Event>> {
        self.rx.try_recv().ok()
    }

    pub fn recv(&self) -> Option<Arc<Event>> {
        self.rx.recv().ok()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Option<Arc<Event>> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Drain everything currently queued, in order.
    pub fn drain(&self) -> Vec<Arc<Event>> {
        self.rx.try_iter().collect()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(inner) = self.bus.upgrade() {
            inner
                .subscribers
                .lock()
                .unwrap()
                .retain(|s| s.id != self.id);
        }
    }
}
