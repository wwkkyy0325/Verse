//! A job that is not a job, for watching the window work.
//!
//! Debug builds only, and only when `VERSE_DEMO=1` is set. It exists because
//! the interface has states that are reached by waiting — a model that is not
//! there, a file that takes minutes, a job that fails — and waiting twenty
//! minutes to look at a progress bar is not a way to develop one.
//!
//! **It drives the real event bus.** Nothing here touches the state machine or
//! the bridge directly: it publishes the same `Event`s a real run publishes, in
//! the same order, and the window reacts through exactly the path it will use
//! in production. A demo that painted the window itself would prove nothing
//! about the window.
//!
//! This is why it cannot ship: a release build has no publisher, so the
//! command's body is `#[cfg]`-compiled out rather than merely unreachable.

use std::path::PathBuf;
use std::time::Duration;

use tauri::{AppHandle, Manager};

use verse_core::{Event, EventBus, JobId, JobKind, Segment, SegmentId, Transcript};

use crate::bridge::App;
use crate::state::Effect;

/// The name the demo job reports.
///
/// Deliberately not the name of anything a person would have. It is a real
/// file, though, and that is not incidental: the automatic save identifies an
/// input by its length and modification time, so a path that does not exist is
/// refused — correctly — and the demonstration would end with a transcript and
/// no file. Which is what it did, until this was made real.
const DEMO_FILE: &str = "演示音频.wav";

/// What the demo reads out, one line at a time.
///
/// Chinese, punctuated, and long enough to fill a transcript pane — which is
/// what the screen is being watched for.
const SCRIPT: [&str; 12] = [
    "好，我们开始今天的会。",
    "先过一下上个月的进度。",
    "整体交付比计划晚了大概两周。",
    "主要卡在硬件到货上。",
    "采购那边说月底能补齐。",
    "那下半年的预算要怎么排？",
    "我建议先把测试设备的钱留出来。",
    "其余的部分按季度分批走。",
    "这个方案我觉得可以。",
    "那就先按这个执行。",
    "有问题随时在群里说。",
    "散会。",
];

/// How long the whole demonstration takes.
const TOTAL: Duration = Duration::from_secs(12);

/// Whether the demonstration was asked for.
pub fn enabled() -> bool {
    std::env::var_os("VERSE_DEMO").is_some_and(|value| value != "0")
}

/// Put the window through a job it did not have to wait for.
///
/// `Ok(false)` when the demonstration was not asked for, which is the ordinary
/// case and not a failure; the error is for being asked and refusing, which is
/// worth showing. Collapsing the two into one would mean either a message on
/// every ordinary launch or a real problem going unreported.
pub fn run(app: AppHandle) -> Result<bool, String> {
    if !enabled() {
        return Ok(false);
    }

    let input = scratch_file()?;

    let shared = app.state::<App>();
    let bus = shared.bus.clone();

    // The state has to be given a file before any event means anything: the
    // pipeline's output is routed to the entry holding the job, and there is
    // none until a file is chosen.
    let effect = {
        let mut state = shared.state.lock().expect("state mutex poisoned");

        // The same path every run, so the second run finds it already in the
        // list — and a file already in the list is *shown* rather than started,
        // which is the right answer to a drop and no answer at all to this.
        // `retry` is the way to run a file that is already there.
        match state.file_chosen(input, true) {
            Effect::None => state.retry(),
            started => started,
        }
    };

    if !matches!(effect, Effect::Transcribe(_)) {
        return Err("现在有别的任务在跑，等它结束再试。".to_string());
    }

    let job = shared.claim_id();
    std::thread::spawn(move || perform(bus, job));

    Ok(true)
}

/// Somewhere for the demonstration's file to live.
///
/// Real, because everything downstream of choosing a file treats it as real:
/// the automatic save stats it, and the output is named after it.
fn scratch_file() -> Result<PathBuf, String> {
    // One path, every run. A per-run path was tried and is worse: the automatic
    // save names a result after its source, so a new path each time meant a new
    // numbered file in the output folder each time, and the demonstration left
    // a trail of them in a person's documents. One path overwrites its own
    // result instead.
    let directory = std::env::temp_dir().join("verse-demo");

    std::fs::create_dir_all(&directory)
        .map_err(|e| format!("建不了演示用的临时目录：{e}"))?;

    let path = directory.join(DEMO_FILE);

    // Written once and then left alone. Rewriting it every run changes its
    // modification time, and the automatic save identifies a source by
    // path, length *and* modification time — so a rewritten placeholder is a
    // *different* source, and every run produced another numbered file in the
    // output folder. Fixed the path and not the identity, which fixed nothing.
    if !path.exists() {
        std::fs::write(
            &path,
            b"a placeholder, so the demonstration has something real to name",
        )
        .map_err(|e| format!("写不了演示用的文件：{e}"))?;
    }

    Ok(path)
}

/// The shape of a real run, at a speed a person can watch.
fn perform(bus: EventBus, job: JobId) {
    bus.publish(Event::JobStarted {
        id: job,
        kind: JobKind::FileTranscribe,
    });

    let total = SCRIPT.len() as u32;
    let pause = TOTAL / total;

    for (index, line) in SCRIPT.iter().enumerate() {
        std::thread::sleep(pause);

        let start = pause * index as u32;
        let end = pause * (index as u32 + 1);

        bus.publish(Event::TranscriptSegment {
            job,
            segment: Segment {
                id: SegmentId(index as u64),
                start,
                end,
                text: (*line).to_string(),
            },
        });

        // After the segment, so the transcript is already longer when the bar
        // moves — the order a real run produces, where a span is recognised
        // and then reported.
        bus.publish(Event::JobProgress {
            id: job,
            position: end,
            fraction: Some((index as f32 + 1.0) / total as f32),
        });
    }

    bus.publish(Event::TranscriptFinal {
        job,
        transcript: Transcript {
            segments: SCRIPT
                .iter()
                .enumerate()
                .map(|(index, line)| Segment {
                    id: SegmentId(index as u64),
                    start: pause * index as u32,
                    end: pause * (index as u32 + 1),
                    text: (*line).to_string(),
                })
                .collect(),
            language: None,
        },
    });
    bus.publish(Event::JobFinished { id: job });
}
