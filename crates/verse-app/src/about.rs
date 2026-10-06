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
    fn about_serializes_as_the_window_expects() {
        let value = serde_json::to_value(about()).expect("serializes");

        assert_eq!(value["name"], json!("Verse"));
        assert_eq!(value["attributions"][0]["what"], json!("语音识别模型"));
        assert!(value["version"].is_string());
    }
}
