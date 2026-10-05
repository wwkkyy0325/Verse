//! Verse core.
//!
//! Holds the domain model, the traits every pluggable component implements,
//! and the communication backbone: registry, router, event bus.
//!
//! This crate contains **no I/O and no FFI**. That is deliberate — network
//! access lives in `verse-model`, engine implementations live in `verse-asr` —
//! so its tests stay fast and its dependency graph stays clean.

pub mod domain;
pub mod error;
pub mod event;
pub mod registry;
pub mod router;
pub mod text;
pub mod traits;

pub use domain::{
    AudioChunk, AudioFormat, HardwareProfile, JobId, ModelId, ModelState, Segment, SegmentId,
    Transcript, TranscriptDelta,
};
pub use error::{Error, ErrorInfo, ErrorKind, Result};
pub use event::{Event, EventBus, JobKind, Subscription};
pub use registry::{
    EngineConfig, EngineDescriptor, Registry, SinkConfig, SinkDescriptor, SourceConfig,
    SourceDescriptor,
};
pub use router::{CancelToken, Job, JobInput, Pipeline, Router, RuntimeContext};
pub use text::{TextChain, TextProcessor};
pub use traits::{AsrEngine, AudioSource, TextSink};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    // ---- stubs --------------------------------------------------------

    struct StubEngine {
        accepted: usize,
    }

    impl AsrEngine for StubEngine {
        fn is_streaming(&self) -> bool {
            false
        }
        fn accept(&mut self, _chunk: &AudioChunk) -> Result<()> {
            self.accepted += 1;
            Ok(())
        }
        fn poll(&mut self) -> Result<Option<TranscriptDelta>> {
            Ok(None)
        }
        fn finalize(&mut self) -> Result<Transcript> {
            Ok(Transcript {
                segments: vec![Segment {
                    id: SegmentId(1),
                    start: Duration::ZERO,
                    end: Duration::from_secs(1),
                    text: "stub".to_string(),
                }],
                language: Some("zh".to_string()),
            })
        }
        fn reset(&mut self) {
            self.accepted = 0;
        }
    }

    /// Walks four progress steps, checking cancellation between each.
    struct StubPipeline;

    impl Pipeline for StubPipeline {
        fn kind(&self) -> JobKind {
            JobKind::FileTranscribe
        }

        fn run(&mut self, job: Job, ctx: &RuntimeContext) -> Result<()> {
            ctx.events.publish(Event::JobStarted {
                id: job.id,
                kind: job.kind,
            });
            for step in 0..4u32 {
                ctx.cancel.check()?;
                ctx.events.publish(Event::JobProgress {
                    id: job.id,
                    position: Duration::from_millis(u64::from(step) * 250),
                    fraction: (step + 1) as f32 / 4.0,
                });
            }
            ctx.events.publish(Event::TranscriptFinal {
                job: job.id,
                transcript: Transcript::default(),
            });
            ctx.events.publish(Event::JobFinished { id: job.id });
            Ok(())
        }
    }

    fn stub_context() -> RuntimeContext {
        RuntimeContext::new(Arc::new(Registry::new()), EventBus::new())
    }

    fn stub_router() -> Router {
        let mut router = Router::new();
        router.register(JobKind::FileTranscribe, || Box::new(StubPipeline));
        router
    }

    fn file_job() -> Job {
        Job::new(
            JobId(1),
            JobKind::FileTranscribe,
            JobInput::File(PathBuf::from("a.mp3")),
        )
    }

    // ---- event bus ----------------------------------------------------

    #[test]
    fn events_reach_only_matching_subscribers() {
        let bus = EventBus::new();
        let for_job_1 = bus.subscribe(|e| e.job() == Some(JobId(1)));
        let everything = bus.subscribe_all();

        bus.publish(Event::JobStarted {
            id: JobId(1),
            kind: JobKind::FileTranscribe,
        });
        bus.publish(Event::JobStarted {
            id: JobId(2),
            kind: JobKind::LiveSubtitle,
        });
        bus.publish(Event::HardwareProbed {
            profile: HardwareProfile {
                avx2: true,
                fma: true,
                cores: 8,
            },
        });

        assert_eq!(for_job_1.drain().len(), 1);
        assert_eq!(everything.drain().len(), 3);
    }

    #[test]
    fn dropping_a_subscription_unsubscribes() {
        let bus = EventBus::new();
        {
            let _sub = bus.subscribe_all();
            assert_eq!(bus.subscriber_count(), 1);
        }
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[test]
    fn jobless_events_are_not_attributed_to_a_job() {
        let bus = EventBus::new();
        let for_job_1 = bus.subscribe(|e| e.job() == Some(JobId(1)));
        bus.publish(Event::ModelStateChanged {
            model: ModelId("paraformer-zh"),
            state: ModelState::Ready,
        });
        assert!(for_job_1.drain().is_empty());
    }

    // ---- registry -----------------------------------------------------

    #[test]
    fn registry_resolves_a_registered_factory() {
        let mut registry = Registry::new();
        registry.register_engine(EngineDescriptor {
            id: "stub",
            display_name: "Stub engine",
            streaming: false,
            factory: Arc::new(
                |_cfg| Ok(Box::new(StubEngine { accepted: 0 }) as Box<dyn AsrEngine>),
            ),
        });

        let cfg = EngineConfig {
            model_dir: PathBuf::from("."),
            threads: 2,
        };
        let engine = registry
            .create_engine("stub", &cfg)
            .expect("registered engine resolves");
        assert!(!engine.is_streaming());

        let err = registry
            .create_engine("nope", &cfg)
            .err()
            .expect("unknown id fails");
        assert_eq!(err.kind(), ErrorKind::Registry);
        assert!(
            err.message().contains("nope"),
            "message should name the missing id"
        );
    }

    #[test]
    fn registry_lists_what_is_registered() {
        let mut registry = Registry::new();
        registry.register_engine(EngineDescriptor {
            id: "stub",
            display_name: "Stub engine",
            streaming: false,
            factory: Arc::new(
                |_cfg| Ok(Box::new(StubEngine { accepted: 0 }) as Box<dyn AsrEngine>),
            ),
        });

        let ids: Vec<_> = registry.engines().map(|d| d.id).collect();
        assert_eq!(ids, vec!["stub"]);
    }

    // ---- router -------------------------------------------------------

    #[test]
    fn pipeline_emits_progress_then_finishes() {
        let ctx = stub_context();
        let sub = ctx.events.subscribe_all();

        stub_router()
            .run(&file_job(), &ctx)
            .expect("stub pipeline succeeds");

        let events = sub.drain();
        assert!(
            matches!(
                events.first().map(|e| e.as_ref()),
                Some(Event::JobStarted { .. })
            ),
            "first event should be JobStarted"
        );
        assert!(
            matches!(
                events.last().map(|e| e.as_ref()),
                Some(Event::JobFinished { .. })
            ),
            "last event should be JobFinished"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.as_ref(), Event::JobProgress { .. }))
                .count(),
            4
        );
    }

    #[test]
    fn cancellation_stops_the_pipeline_before_completion() {
        let ctx = stub_context();
        let sub = ctx.events.subscribe_all();
        ctx.cancel.cancel();

        let err = stub_router()
            .run(&file_job(), &ctx)
            .expect_err("cancelled job fails");
        assert_eq!(err.kind(), ErrorKind::Cancelled);
        assert!(
            !sub.drain()
                .iter()
                .any(|e| matches!(e.as_ref(), Event::JobFinished { .. })),
            "a cancelled job must not report completion"
        );
    }

    #[test]
    fn unregistered_job_kind_is_reported_clearly() {
        let ctx = stub_context();
        let job = Job::new(JobId(9), JobKind::LiveSubtitle, JobInput::Live);

        let err = stub_router()
            .run(&job, &ctx)
            .expect_err("unregistered kind fails");
        assert_eq!(err.kind(), ErrorKind::Registry);
        assert!(
            err.message().contains("LiveSubtitle"),
            "message should name the missing kind"
        );
    }

    #[test]
    fn each_job_gets_a_fresh_pipeline() {
        let ctx = stub_context();
        let router = stub_router();

        // Running twice must not carry state across; a factory per job is what
        // guarantees it.
        router.run(&file_job(), &ctx).expect("first run");
        router.run(&file_job(), &ctx).expect("second run");
    }
}
