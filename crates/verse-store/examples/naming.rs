//! What the output rules actually do, printed.
//!
//! `cargo run -p verse-store --example naming`
//!
//! Everything below happens in a temporary directory. The point is to show the
//! sequence a person would see in their own output folder — `会议.srt`, then
//! `会议 (2).srt`, then the first one again — rather than to assert it, which
//! the unit tests already do.

use std::path::{Path, PathBuf};

use verse_store::{destination, output_dir, Ownership, Roots};

fn main() {
    let root = std::env::temp_dir().join("verse-store-example");
    let _ = std::fs::remove_dir_all(&root);
    let out = root.join("output");
    std::fs::create_dir_all(&out).expect("create output dir");

    // Built by joining rather than from string literals, so the separators are
    // this platform's and the illustration does not read as a bug.
    let home = PathBuf::from("home").join("me");
    let documents = home.join("Documents");

    println!("output directory (with no overrides):");
    for (label, roots) in [
        (
            "the ordinary case",
            Roots {
                documents: Some(documents.clone()),
                ..Roots::default()
            },
        ),
        (
            "Documents missing",
            Roots {
                home: Some(home.clone()),
                ..Roots::default()
            },
        ),
        (
            "nothing at all",
            Roots::default(),
        ),
        (
            "VERSE_OUTPUT set",
            Roots {
                output: Some(PathBuf::from("elsewhere")),
                documents: Some(documents.clone()),
                ..Roots::default()
            },
        ),
    ] {
        println!("  {label:<20} {}", output_dir(&roots).display());
    }
    println!();

    // Two recordings, each called 会议.m4a, from different folders. This is the
    // case that used to be impossible and is now ordinary.
    let mut ownership = Ownership::default();
    let first = source(&root, "january/会议.m4a");
    let second = source(&root, "february/会议.m4a");

    println!("writing into an empty folder:");
    let a = destination(&out, "会议".as_ref(), "srt", &first, &ownership);
    write(&a.path, "january's transcript");
    ownership.record(&first, &a.path);
    println!("  january   -> {}", name(&a.path));

    let b = destination(&out, "会议".as_ref(), "srt", &second, &ownership);
    write(&b.path, "february's transcript");
    ownership.record(&second, &b.path);
    println!("  february  -> {}", name(&b.path));

    println!();
    println!("running both again, unchanged:");
    let a2 = destination(&out, "会议".as_ref(), "srt", &first, &ownership);
    let b2 = destination(&out, "会议".as_ref(), "srt", &second, &ownership);
    println!("  january   -> {}", name(&a2.path));
    println!("  february  -> {}", name(&b2.path));

    let files: Vec<String> = std::fs::read_dir(&out)
        .expect("list output")
        .map(|entry| name(&entry.expect("entry").path()))
        .collect();
    println!("  folder now holds {} files: {files:?}", files.len());
    println!(
        "  january's text is still {}",
        std::fs::read_to_string(&a.path).expect("read")
    );

    // A file nobody recorded. Not ours to replace.
    println!();
    println!("a file that appeared without a record:");
    write(&out.join("手写.srt"), "typed by hand");
    let third = source(&root, "march/手写.m4a");
    let c = destination(&out, "手写".as_ref(), "srt", &third, &ownership);
    println!("  march     -> {}", name(&c.path));
    println!(
        "  the hand-written file still says: {}",
        std::fs::read_to_string(out.join("手写.srt")).expect("read")
    );

    let _ = std::fs::remove_dir_all(&root);
}

fn source(root: &Path, relative: &str) -> verse_store::FileId {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    std::fs::write(&path, b"pretend audio").expect("write input");
    verse_store::FileId::of(&path).expect("stat input")
}

fn write(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("write");
}

fn name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
