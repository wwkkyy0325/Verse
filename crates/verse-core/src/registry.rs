use crate::error::{Error, ErrorKind, Result};
use crate::traits::{AsrEngine, AudioSource, TextSink};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Configuration handed to an engine factory.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Directory containing the model files.
    pub model_dir: PathBuf,
    pub threads: usize,
    /// Whether to normalise written forms: 十七万 to 17万, 百分之四 to 4%.
    ///
    /// Exposed because it is not a recognition decision. Turning it off
    /// changes how a number is written, never whether it was heard — so a
    /// benchmark comparing against transcripts that spell numbers out has to
    /// be able to switch it off, or it scores formatting instead of accuracy.
    ///
    /// Engines without such a mode ignore it.
    pub inverse_text_normalization: bool,
}

/// Configuration handed to a source factory.
#[derive(Debug, Clone, Default)]
pub struct SourceConfig {
    /// Input file, for file-backed sources. `None` for live capture.
    pub path: Option<PathBuf>,
}

/// Configuration handed to a sink factory.
#[derive(Debug, Clone, Default)]
pub struct SinkConfig {
    /// Output file, for file-backed sinks.
    pub path: Option<PathBuf>,
}

pub type EngineFactory = Arc<dyn Fn(&EngineConfig) -> Result<Box<dyn AsrEngine>> + Send + Sync>;
pub type SourceFactory = Arc<dyn Fn(&SourceConfig) -> Result<Box<dyn AudioSource>> + Send + Sync>;
pub type SinkFactory = Arc<dyn Fn(&SinkConfig) -> Result<Box<dyn TextSink>> + Send + Sync>;

/// A registered recognition backend.
pub struct EngineDescriptor {
    pub id: &'static str,
    /// Human-readable name, for settings UI.
    pub display_name: &'static str,
    /// Whether this engine emits incremental output. Lets the UI decide
    /// between a progress bar and a live text view before instantiating it.
    pub streaming: bool,
    pub factory: EngineFactory,
}

/// A registered audio source.
pub struct SourceDescriptor {
    pub id: &'static str,
    pub display_name: &'static str,
    pub factory: SourceFactory,
}

/// A registered text destination.
pub struct SinkDescriptor {
    pub id: &'static str,
    pub display_name: &'static str,
    pub factory: SinkFactory,
}

/// The catalogue of pluggable implementations.
///
/// Adding a Phase 2 engine means calling `register_engine` — orchestration
/// code does not change. Lookup failures are reported as [`ErrorKind::Registry`]
/// with the missing id in the message, so a typo produces a usable diagnostic
/// rather than a panic.
#[derive(Default)]
pub struct Registry {
    engines: HashMap<&'static str, EngineDescriptor>,
    sources: HashMap<&'static str, SourceDescriptor>,
    sinks: HashMap<&'static str, SinkDescriptor>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_engine(&mut self, desc: EngineDescriptor) -> &mut Self {
        self.engines.insert(desc.id, desc);
        self
    }

    pub fn register_source(&mut self, desc: SourceDescriptor) -> &mut Self {
        self.sources.insert(desc.id, desc);
        self
    }

    pub fn register_sink(&mut self, desc: SinkDescriptor) -> &mut Self {
        self.sinks.insert(desc.id, desc);
        self
    }

    pub fn engines(&self) -> impl Iterator<Item = &EngineDescriptor> {
        self.engines.values()
    }

    pub fn engine(&self, id: &str) -> Option<&EngineDescriptor> {
        self.engines.get(id)
    }

    pub fn sources(&self) -> impl Iterator<Item = &SourceDescriptor> {
        self.sources.values()
    }

    pub fn source(&self, id: &str) -> Option<&SourceDescriptor> {
        self.sources.get(id)
    }

    pub fn sinks(&self) -> impl Iterator<Item = &SinkDescriptor> {
        self.sinks.values()
    }

    pub fn sink(&self, id: &str) -> Option<&SinkDescriptor> {
        self.sinks.get(id)
    }

    pub fn create_engine(&self, id: &str, cfg: &EngineConfig) -> Result<Box<dyn AsrEngine>> {
        let desc = self.engines.get(id).ok_or_else(|| missing("engine", id))?;
        (desc.factory)(cfg)
    }

    pub fn create_source(&self, id: &str, cfg: &SourceConfig) -> Result<Box<dyn AudioSource>> {
        let desc = self.sources.get(id).ok_or_else(|| missing("source", id))?;
        (desc.factory)(cfg)
    }

    pub fn create_sink(&self, id: &str, cfg: &SinkConfig) -> Result<Box<dyn TextSink>> {
        let desc = self.sinks.get(id).ok_or_else(|| missing("sink", id))?;
        (desc.factory)(cfg)
    }
}

fn missing(kind: &str, id: &str) -> Error {
    Error::new(
        ErrorKind::Registry,
        format!("no {kind} registered under '{id}'"),
    )
}
