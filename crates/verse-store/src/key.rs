//! What makes two runs the same run.
//!
//! The key has two halves. One describes the **settings** — engine, model
//! files, detector thresholds, every knob that changes what comes out. The
//! other describes the **audio**. Together they say "this exact work has
//! already been done".
//!
//! Getting this wrong in the direction of a false hit is the one unrecoverable
//! mistake available here: a wrong transcript is returned as though it were
//! this file's, and nothing downstream is positioned to notice. So the design
//! leans towards missing rather than matching, and the reasoning for each
//! inclusion is below rather than in a commit message.

use std::fmt::Write as _;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use sha2::{Digest, Sha256};

/// Bumped by hand whenever anything upstream changes what a transcription
/// *contains*.
///
/// Model files are fingerprinted, and settings are fingerprinted, and neither
/// covers a change in the code that turns one into the other — renumbering
/// segments, changing the guard's fallback, fixing a text post-process. Those
/// changes produce different output from identical inputs, so this constant is
/// the only thing that can catch them. Bumping it invalidates every cached
/// entry, which is the correct and cheap response; forgetting to bump it serves
/// stale transcripts forever.
pub const SEMANTICS: u32 = 1;

/// How much of a file to read when deciding whether it has changed.
///
/// Hashing two gigabytes on every run would make the fast path slower than the
/// work it avoids. This is the compromise, and its limits are stated rather
/// than implied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verify {
    /// Length, plus the first and last 256 KiB.
    #[default]
    Bounded,
    /// The whole file. Slower, and the only setting under which a change in
    /// the middle of a large file is certain to be seen.
    Full,
}

/// How much of each end [`Verify::Bounded`] reads.
const WINDOW: u64 = 256 * 1024;

impl Verify {
    /// The mode named by `VERSE_CACHE_VERIFY`.
    ///
    /// Anything unrecognised means the default, including a misspelling.
    /// Reading an unknown word as "full" would silently make every run slow;
    /// reading it as "bounded" is what a person who did not set the variable
    /// gets anyway.
    pub fn from_env() -> Self {
        match std::env::var("VERSE_CACHE_VERIFY").ok().as_deref() {
            Some("full") => Verify::Full,
            _ => Verify::Bounded,
        }
    }
}

/// Whether `VERSE_NO_CACHE` asks for the cache to be left out entirely.
///
/// Set to anything but the empty string. An empty value means "unset", by the
/// same rule [`crate::dirs`] applies to its own variables.
pub fn caching_refused() -> bool {
    std::env::var_os("VERSE_NO_CACHE").is_some_and(|value| !value.is_empty())
}

/// The accumulated description of one run's settings.
///
/// A list of labelled values rather than a struct, because the fields come from
/// three crates — the pipeline's request, the detector's settings, the guard's
/// — and this crate deliberately depends on none of them. The caller describes
/// its own configuration; this hashes the description.
#[derive(Debug, Clone, Default)]
pub struct Fingerprint {
    parts: Vec<(String, String)>,
}

impl Fingerprint {
    pub fn new() -> Self {
        let mut fingerprint = Self::default();
        fingerprint.add("semantics", SEMANTICS);
        fingerprint
    }

    /// Record a value that has a faithful textual form.
    pub fn add(&mut self, label: &str, value: impl std::fmt::Display) {
        self.parts.push((label.to_string(), value.to_string()));
    }

    /// Record a float by its bit pattern rather than by its value.
    ///
    /// `Display` on a float is shortest-round-trip, so it would in fact tell
    /// two different values apart today — this is not a fix for a bug that
    /// exists. Bits are used because that property is a choice the standard
    /// library makes, not a contract this crate can hold it to: the moment a
    /// rendering is rounded, for tidiness in a log or a `{:.4}` added by
    /// someone who wanted the key file to read nicely, two thresholds a
    /// ten-thousandth apart become one string, and a key that cannot tell them
    /// apart serves the other one's transcript.
    pub fn add_f32(&mut self, label: &str, value: f32) {
        self.add(label, format!("f32:{:08x}", value.to_bits()));
    }

    pub fn add_f64(&mut self, label: &str, value: f64) {
        self.add(label, format!("f64:{:016x}", value.to_bits()));
    }

    /// Record a file by where it is, how big it is, and when it was written.
    ///
    /// The contents are not hashed. A model is 228 MB and would be read on
    /// every run to answer a question that length-and-mtime answers in
    /// practice; the case it misses — a model replaced in place, at the same
    /// size, with its timestamp restored — is not one that happens by
    /// accident.
    pub fn add_file(&mut self, label: &str, path: &Path) {
        match std::fs::metadata(path) {
            Ok(metadata) => {
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|since| since.as_nanos().to_string())
                    .unwrap_or_else(|| "-".to_string());

                let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
                self.add(
                    label,
                    format!("{}|{}|{}", canonical.display(), metadata.len(), modified),
                );
            }
            // A file that is not there is its own answer, and distinct from an
            // empty one. Recording nothing here would make a run with a missing
            // model look like a run with a present one.
            Err(_) => self.add(label, "missing"),
        }
    }

    /// The digest of everything recorded so far.
    pub fn digest(&self) -> String {
        let mut parts = self.parts.clone();
        // Sorted, so that a caller which adds the same facts in a different
        // order still produces the same key. Duplicate labels are kept and
        // ordered by value, which keeps the digest a function of the multiset.
        parts.sort();

        let mut hasher = Sha256::new();
        for (label, value) in &parts {
            // Length-prefixed, so that two adjacent fields cannot be read as a
            // different split of the same bytes — ("ab","c") and ("a","bc")
            // must not collide.
            hasher.update((label.len() as u64).to_le_bytes());
            hasher.update(label.as_bytes());
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
        hex(&hasher.finalize())
    }
}

/// A digest of the audio itself.
///
/// Under [`Verify::Bounded`] this covers the length and both ends. A file whose
/// *middle* changed while its length, its timestamps and both end-windows
/// stayed identical would be treated as unchanged. The realistic route to that
/// is restoring a backup that preserved timestamps, with the edit landing in
/// the middle; `VERSE_CACHE_VERIFY=full` exists for a caller who will not
/// accept it.
pub fn content_digest(path: &Path, verify: Verify) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut hasher = Sha256::new();
    hasher.update(len.to_le_bytes());

    match verify {
        Verify::Full => {
            copy_into(&mut file, &mut hasher, len)?;
        }
        Verify::Bounded => {
            if len <= WINDOW * 2 {
                // Shorter than the two windows, so reading "both ends" would
                // read the middle twice and the seam would be arbitrary.
                copy_into(&mut file, &mut hasher, len)?;
            } else {
                copy_into(&mut file, &mut hasher, WINDOW)?;
                file.seek(SeekFrom::End(-(WINDOW as i64)))?;
                copy_into(&mut file, &mut hasher, WINDOW)?;
            }
        }
    }

    Ok(hex(&hasher.finalize()))
}

/// Read `count` bytes into the hasher, stopping early at the end of the file.
fn copy_into(
    file: &mut std::fs::File,
    hasher: &mut Sha256,
    count: u64,
) -> std::io::Result<()> {
    let mut remaining = count;
    let mut buffer = vec![0u8; 64 * 1024];

    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        let read = file.read(&mut buffer[..want])?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    Ok(())
}

/// The key for one run: the settings and the audio, together.
pub fn key(settings: &str, content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(settings.as_bytes());
    hasher.update(b"|");
    hasher.update(content.as_bytes());
    hex(&hasher.finalize())
}

/// Lower-case hex.
///
/// Written out rather than pulled in: the `hex` crate would be a dependency for
/// eight lines, in a project that hand-rolls its argument parsing to avoid one.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("verse-store-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn the_same_settings_give_the_same_key() {
        let mut first = Fingerprint::new();
        first.add("engine", "sensevoice");
        first.add("threads", 7);

        let mut second = Fingerprint::new();
        second.add("threads", 7);
        second.add("engine", "sensevoice");

        assert_eq!(
            first.digest(),
            second.digest(),
            "the order facts were recorded in is not a fact about the run"
        );
    }

    #[test]
    fn a_changed_setting_busts_the_key() {
        let mut before = Fingerprint::new();
        before.add("engine", "sensevoice");

        let mut after = Fingerprint::new();
        after.add("engine", "qwen3-asr");

        assert_ne!(before.digest(), after.digest());
    }

    #[test]
    fn two_labels_cannot_be_resplit_into_a_different_pair() {
        // Without length prefixes ("ab","c") and ("a","bc") hash to the same
        // bytes, so two unrelated configurations would share a cache entry.
        let mut first = Fingerprint::new();
        first.add("ab", "c");

        let mut second = Fingerprint::new();
        second.add("a", "bc");

        assert_ne!(first.digest(), second.digest());
    }

    #[test]
    fn a_changed_model_file_busts_the_key() {
        let dir = scratch("key-model");
        let model = dir.join("model.onnx");
        std::fs::write(&model, b"first version").expect("write");

        let mut before = Fingerprint::new();
        before.add_file("model", &model);

        // Same name, same place, different bytes and a later timestamp.
        std::fs::write(&model, b"second version!").expect("rewrite");

        let mut after = Fingerprint::new();
        after.add_file("model", &model);

        assert_ne!(before.digest(), after.digest());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_model_is_not_the_same_as_a_present_one() {
        let dir = scratch("key-missing-model");
        let present = dir.join("present.onnx");
        std::fs::write(&present, b"x").expect("write");

        let mut with = Fingerprint::new();
        with.add_file("model", &present);
        let mut without = Fingerprint::new();
        without.add_file("model", &dir.join("absent.onnx"));

        assert_ne!(with.digest(), without.digest());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_floats_a_rendering_cannot_separate_still_get_different_keys() {
        // The reason floats go in by bit pattern. Note what this test does *not*
        // claim: `Display` separates these two perfectly well, because it prints
        // the shortest string that round-trips. What does not separate them is a
        // rendering rounded for readability, which is what a key built out of
        // formatted strings invites the next person to introduce.
        let near: f32 = 0.05;
        let nearer: f32 = 0.050_000_1;

        assert_ne!(near.to_bits(), nearer.to_bits(), "these really are two values");
        assert_eq!(
            format!("{near:.4}"),
            format!("{nearer:.4}"),
            "a rounded rendering makes them one string, and this test would be \
             meaningless if it did not"
        );

        let mut first = Fingerprint::new();
        first.add_f32("threshold", near);

        let mut second = Fingerprint::new();
        second.add_f32("threshold", nearer);

        assert_ne!(
            first.digest(),
            second.digest(),
            "a key that cannot tell these apart serves the wrong transcript"
        );
    }

    #[test]
    fn the_settings_and_the_audio_are_both_in_the_key() {
        let a = key("settings-one", "content-one");
        assert_ne!(a, key("settings-two", "content-one"));
        assert_ne!(a, key("settings-one", "content-two"));
        assert_eq!(a, key("settings-one", "content-one"));
    }

    #[test]
    fn the_bounded_digest_notices_a_changed_end() {
        let dir = scratch("key-bounded-end");
        let file = dir.join("a.bin");
        let body = vec![b'a'; 700 * 1024];

        std::fs::write(&file, &body).expect("write");
        let before = content_digest(&file, Verify::Bounded).expect("digest");

        let mut changed = body.clone();
        changed[0] = b'b';
        std::fs::write(&file, &changed).expect("rewrite");
        let after = content_digest(&file, Verify::Bounded).expect("digest");

        assert_ne!(before, after, "the head is inside the window");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_bounded_digest_misses_a_changed_middle_and_full_catches_it() {
        // This is the documented limit, asserted rather than described. If it
        // ever starts passing, the window got bigger and the doc needs to say
        // so.
        let dir = scratch("key-bounded-middle");
        let file = dir.join("a.bin");
        let mut body = vec![b'a'; 700 * 1024];

        std::fs::write(&file, &body).expect("write");
        let bounded_before = content_digest(&file, Verify::Bounded).expect("digest");
        let full_before = content_digest(&file, Verify::Full).expect("digest");

        body[350 * 1024] = b'b';
        std::fs::write(&file, &body).expect("rewrite");

        assert_eq!(
            bounded_before,
            content_digest(&file, Verify::Bounded).expect("digest"),
            "the documented limit: a middle-only change is not seen"
        );
        assert_ne!(
            full_before,
            content_digest(&file, Verify::Full).expect("digest"),
            "full verification is the answer for a caller who will not accept it"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_short_file_is_hashed_once_rather_than_twice() {
        // Below two windows the two windows overlap, and hashing "both ends"
        // would read the middle twice.
        let dir = scratch("key-short");
        let file = dir.join("a.bin");
        std::fs::write(&file, b"small").expect("write");

        let digest = content_digest(&file, Verify::Bounded).expect("digest");

        let mut expected = Sha256::new();
        expected.update(5u64.to_le_bytes());
        expected.update(b"small");
        assert_eq!(digest, hex(&expected.finalize()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_length_alone_is_enough() {
        let dir = scratch("key-length");
        let file = dir.join("a.bin");
        std::fs::write(&file, b"abc").expect("write");
        let before = content_digest(&file, Verify::Bounded).expect("digest");

        std::fs::write(&file, b"abcd").expect("rewrite");
        let after = content_digest(&file, Verify::Bounded).expect("digest");

        assert_ne!(before, after);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_semantics_constant_is_in_every_key() {
        // It is the only thing that can catch a change in the code that turns
        // inputs into output, which no model file or setting reflects.
        assert_eq!(SEMANTICS, 1, "bumping this invalidates every entry; that is the point");
        let mut fingerprint = Fingerprint::new();
        assert_eq!(fingerprint.digest(), Fingerprint::new().digest());
        fingerprint.add("engine", "sensevoice");
        assert_ne!(fingerprint.digest(), Fingerprint::new().digest());
    }

    #[test]
    fn hex_is_lower_case_and_full_width() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
    }
}
