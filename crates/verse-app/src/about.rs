//! What the window says about the models it uses.
//!
//! The FunASR model licence requires attribution to Alibaba Group /
//! FunAudioLLM and that the name "SenseVoiceSmall" is kept verbatim wherever
//! the model is named. Both are distribution obligations, not courtesies, so
//! they live in one place that a test can hold to.
//!
//! `THIRD_PARTY_NOTICES.md` is the authority; this is what the interface shows.

use serde::Serialize;

/// One line of required attribution.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attribution {
    /// What the row is about, in the user's words.
    pub what: &'static str,
    pub who: &'static str,
}

/// One route of the local service, as the dialog lists it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Route {
    pub method: &'static str,
    pub path: &'static str,
    /// What it is for, in the user's words.
    pub what: &'static str,
}

/// The service's routes.
///
/// **One list, two renderings**: the dialog shows these rows, and the brief an
/// agent is handed prints the same rows. Generated from one place, so the two
/// cannot describe different APIs.
///
/// **What this does not prove.** These are a copy of `crates/verse-cli`'s
/// router, which lives in another crate and reaches this one only as a running
/// process — the CLI has no library target to import. Nothing here is checked
/// against it, so a route added there and not here would be documented wrong.
/// `llms.txt` has the same exposure. Recorded rather than papered over.
pub const ROUTES: &[Route] = &[
    Route {
        method: "GET",
        path: "/health",
        what: "这个服务是什么、正在做什么、池子有多大",
    },
    Route {
        method: "GET",
        path: "/models",
        what: "模型清单",
    },
    Route {
        method: "POST",
        path: "/jobs",
        what: "交一个转写任务，返回 202 和 Location",
    },
    Route {
        method: "GET",
        path: "/jobs",
        what: "手上的全部任务，最新在前",
    },
    Route {
        method: "GET",
        path: "/jobs/<id>",
        what: "一个任务：状态、进度、结果",
    },
    Route {
        method: "DELETE",
        path: "/jobs/<id>",
        what: "取消一个任务",
    },
];

/// What opens it, and what it needs before the routes below mean anything.
const BRIEF_HEAD: &str = "Verse runs a local speech-to-text service. Nothing leaves the machine, and the
service initiates no connection of its own.

Start it with the `verse` command-line tool that ships beside this application:

    verse serve [--port <n>] [--models <dir>] [--engine sensevoice|qwen3-asr]

It listens on 127.0.0.1 and writes a discovery file to the per-user data
directory (VERSE_CACHE overrides which directory):

    serve.json   { host, port, token, pid, instance, startedAtMs, modelsDir }

Read the port and the token from there. If the file may be stale, compare its
`instance` against /health before trusting the port.

Every request must carry:

    Authorization: Bearer <token>

A request carrying an Origin header is refused with 403: the clients are
programs, not browsers.

`/health` says what the service is, what it is doing and how big its worker pool
is. `/models` is the catalogue. The rest concern jobs:

";

/// What the routes mean once a job is in flight.
const BRIEF_TAIL: &str = "\
POST /jobs takes JSON:

    {\"input\": \"<absolute path>\", \"format\": \"srt|txt\",
     \"engine\": \"sensevoice|qwen3-asr\", \"hotwords\": \"...\", \"noCache\": false}

It answers 202 with a `Location: /jobs/<id>` header. Poll GET /jobs/<id> until
`state` is one of `done`, `failed` or `cancelled`; then `result` is the same
FileResult the command line's `--json` emits, so one parser reads both.

`progress.fraction` is how far through the file, or null when the input
declares no length. Null is not zero: zero means the beginning.

Several jobs run at once, one per worker. The queue holds 32 and refuses beyond
that with 429; finished jobs are kept for the most recent 32, so polling a very
old id is a 404 whose message says so.

Finished transcripts are also written to the output directory as they land
(Documents/Verse by default, VERSE_OUTPUT overrides), so a workflow can pick up
a file instead of reading JSON.
";

/// The brief, assembled from the same rows the dialog shows.
///
/// Method and path only, with no description: the descriptions are Chinese
/// because they are read by a person looking at the dialog, and this text is
/// read by a program. Printing them here would make the brief half English and
/// half Chinese — which is what it did before this comment was written to
/// match. What a route is *for* is said in prose around the list instead, once,
/// in the language the brief is in.
pub fn agent_brief() -> String {
    let mut brief = String::from(BRIEF_HEAD);
    for route in ROUTES {
        brief.push_str(&format!("    {:<6} {}\n", route.method, route.path));
    }
    brief.push('\n');
    brief.push_str(BRIEF_TAIL);
    brief
}

/// Everything the 关于 dialog shows.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct About {
    pub name: &'static str,
    pub version: &'static str,
    pub summary: &'static str,
    pub attributions: Vec<Attribution>,
    /// Shown under the list. Deliberately plain about what this build does and
    /// does not carry.
    pub licence_note: &'static str,
    /// The service's routes, for the dialog to list.
    pub routes: Vec<Route>,
    /// The same thing written out for an agent to be handed, in English.
    ///
    /// English while everything around it is Chinese, and that is deliberate:
    /// this text is not for the person reading the dialog, it is an interface
    /// description for whatever program they paste it into, and it matches the
    /// voice of `llms.txt`. The dialog labels it in Chinese and says what it is
    /// for.
    pub agent_brief: String,
}

/// The rows the model licence requires, plus the two the project owes on its
/// own account.
///
/// "SenseVoiceSmall" is spelled exactly as upstream spells it. The licence
/// requires the name be retained, and a tidied-up rendering would not be it.
pub const ATTRIBUTIONS: &[Attribution] = &[
    Attribution {
        what: "语音识别模型",
        who: "SenseVoiceSmall — 阿里巴巴集团 FunAudioLLM 团队",
    },
    Attribution {
        what: "模型许可证",
        who: "FunASR Model Open Source License Agreement v1.1",
    },
    Attribution {
        what: "语音检测模型",
        who: "Silero VAD",
    },
    Attribution {
        what: "语音识别运行时",
        who: "sherpa-onnx",
    },
    Attribution {
        what: "媒体解码",
        who: "FFmpeg（以独立进程调用）",
    },
];

/// Said out loud rather than implied.
///
/// The agreement requires its own text to ship with the product, and it does:
/// `licences/` holds all three verbatim, and `tauri.conf.json` lists them under
/// `bundle.resources` so they reach the installer rather than only the
/// repository. §6 lets the agreement be revised unilaterally, so the archived
/// copy with its SHA-256 is the evidence of the terms we accepted.
///
/// What still does not ship is the weights. They are fetched on demand, and
/// that difference is worth stating because "the licence is included" reads
/// like "the model is included".
pub const LICENCE_NOTE: &str = "模型权重不随安装包分发，由你自己下载后保存在本机；\
识别全部在本机完成。许可证全文随安装包附带，见安装目录下的 licences 文件夹。";

pub fn about() -> About {
    About {
        name: "Verse",
        version: env!("CARGO_PKG_VERSION"),
        summary: "本地语音转文字。识别在本机完成，不联网，不上传。",
        attributions: ATTRIBUTIONS
            .iter()
            .map(|row| Attribution {
                what: row.what,
                who: row.who,
            })
            .collect(),
        licence_note: LICENCE_NOTE,
        routes: ROUTES
            .iter()
            .map(|route| Route {
                method: route.method,
                path: route.path,
                what: route.what,
            })
            .collect(),
        agent_brief: agent_brief(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_required_attributions_are_present() {
        let rows = about().attributions;
        let joined = rows
            .iter()
            .map(|row| format!("{} {}", row.what, row.who))
            .collect::<Vec<_>>()
            .join("\n");

        // The three the model licence requires, and that a tidying pass is
        // not allowed to drop.
        assert!(joined.contains("阿里巴巴"), "the attribution to Alibaba");
        assert!(joined.contains("FunAudioLLM"), "the attribution to FunAudioLLM");
        assert!(
            joined.contains("SenseVoiceSmall"),
            "the name must be retained verbatim"
        );
        assert!(joined.contains("FunASR Model"), "the licence must be named");
    }

    #[test]
    fn the_name_is_not_tidied_up() {
        // "SenseVoice-Small" and "SenseVoice Small" are not the name the
        // licence requires be kept.
        let rows = about().attributions;
        assert!(rows.iter().any(|row| row.who.contains("SenseVoiceSmall")));
    }

    #[test]
    fn the_licence_note_says_where_the_text_is() {
        // It used to say the text was 尚未内置, which was true and is not any
        // more. The note points at the directory the installer puts it in, and
        // it does not claim the agreement is absent.
        let note = about().licence_note;
        assert!(note.contains("licences"), "the note names where the text is");
        assert!(
            !note.contains("尚未内置"),
            "the text ships now; saying otherwise is a stale claim"
        );
    }

    #[test]
    fn the_brief_carries_every_route_the_dialog_lists() {
        // Not a tautology despite being generated: it pins that the generation
        // is still wired up, and that a route added to `ROUTES` reaches the
        // text somebody will paste into an agent.
        let brief = about().agent_brief;

        for route in ROUTES {
            assert!(
                brief.contains(route.path),
                "{} is listed in the dialog and missing from the brief",
                route.path
            );
        }
    }

    #[test]
    fn the_brief_says_what_it_needs_to_be_usable() {
        // A brief that named the routes but not the token, or not where the
        // port comes from, would be a list of endpoints an agent cannot reach.
        let brief = about().agent_brief;

        for needed in [
            "verse serve",
            "127.0.0.1",
            "serve.json",
            "Authorization: Bearer",
            "202",
            "fraction",
        ] {
            assert!(brief.contains(needed), "the brief never mentions {needed:?}");
        }
    }

    #[test]
    fn the_brief_is_not_a_half_finished_document() {
        // The text is built in pieces by hand, and a botched escape leaves a
        // real newline where a backslash-n was meant — which still compiles and
        // reads as a broken table. This is the cheap guard against that.
        let brief = about().agent_brief;

        for wrong in ["TODO", "\\n", "\t\t", "[object"] {
            assert!(
                !brief.contains(wrong),
                "the brief contains {wrong:?} literally"
            );
        }
        assert!(
            !brief
                .lines()
                .any(|line| !line.is_empty() && line.trim().is_empty()),
            "no line should be whitespace only"
        );
        assert!(
            brief.lines().count() > 20,
            "the brief is {} lines; it should read like a document",
            brief.lines().count()
        );
    }

    #[test]
    fn about_serializes_as_the_window_expects() {
        let value = serde_json::to_value(about()).expect("serializes");

        assert_eq!(value["name"], json!("Verse"));
        assert_eq!(value["attributions"][0]["what"], json!("语音识别模型"));
        assert!(value["version"].is_string());
    }
}
