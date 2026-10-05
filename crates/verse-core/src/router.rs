use crate::domain::JobId;
use crate::error::{Error, ErrorKind, Result};
use crate::event::{EventBus, JobKind};
use crate::registry::Registry;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// What a job operates on.
#[derive(Debug, Clone)]
pub enum JobInput {
    /// An audio or video file to decode and transcribe.
    File(PathBuf),
    /// Live capture; the pipeline picks the device.
    Live,
    /// Text to translate, for [`JobKind::Translate`].
    Text(String),
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: JobId,
    pub kind: JobKind,
    pub input: JobInput,
}

impl Job {
    pub fn new(id: JobId, kind: JobKind, input: JobInput) -> Self {
        Self { id, kind, input }
    }
}

/// Cooperative cancellation.
///
/// Pipelines poll this between work units and return [`ErrorKind::Cancelled`]
/// when set. There is no forced abort: a cancelled job stops at the next
/// checkpoint, which is what keeps model resources and partial output clean.
#[derive(Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// `Err(Cancelled)` if cancellation was requested, otherwise `Ok(())`.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::cancelled())
        } else {
            Ok(())
        }
    }
}

/// Everything a pipeline needs from its environment. Pipelines never reach for
/// globals; this is the whole surface.
pub struct RuntimeContext {
    pub registry: Arc<Registry>,
    pub events: EventBus,
    pub cancel: CancelToken,
}

impl RuntimeContext {
    pub fn new(registry: Arc<Registry>, events: EventBus) -> Self {
        Self {
            registry,
            events,
            cancel: CancelToken::new(),
        }
    }
}

/// One execution chain for one [`JobKind`].
pub trait Pipeline: Send {
    fn kind(&self) -> JobKind;

    fn run(&mut self, job: Job, ctx: &RuntimeContext) -> Result<()>;
}

type PipelineFactory = Box<dyn Fn() -> Box<dyn Pipeline> + Send + Sync>;

/// Maps job kinds to pipelines.
///
/// Registering a kind is what makes it runnable. An unregistered kind fails at
/// submit time with the kind named in the message, rather than silently doing
/// nothing.
#[derive(Default)]
pub struct Router {
    pipelines: HashMap<JobKind, PipelineFactory>,
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        kind: JobKind,
        factory: impl Fn() -> Box<dyn Pipeline> + Send + Sync + 'static,
    ) -> &mut Self {
        self.pipelines.insert(kind, Box::new(factory));
        self
    }

    pub fn is_registered(&self, kind: JobKind) -> bool {
        self.pipelines.contains_key(&kind)
    }

    pub fn kinds(&self) -> Vec<JobKind> {
        self.pipelines.keys().copied().collect()
    }

    /// Run a job to completion on the calling thread.
    ///
    /// A fresh pipeline is built per job, so pipelines may hold per-job state
    /// without being `Sync` or needing to reset themselves. Running on a
    /// background thread is the caller's choice — the router takes no position
    /// on threading.
    pub fn run(&self, job: &Job, ctx: &RuntimeContext) -> Result<()> {
        let factory = self.pipelines.get(&job.kind).ok_or_else(|| {
            Error::new(
                ErrorKind::Registry,
                format!("no pipeline registered for {:?}", job.kind),
            )
        })?;
        let mut pipeline = factory();
        pipeline.run(job.clone(), ctx)
    }
}
