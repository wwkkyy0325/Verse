"""Unpack a HuggingFace audio parquet into wav files plus a manifest.

Run once per dataset. The manifest is what `verse-bench` reads; keeping the
two steps separate means a dataset is unpacked once and scored many times,
which matters when the scoring step is the slow one.

    python tools/asr-eval/extract.py <parquet> <out-dir> [--reference FIELD]

The manifest is `<id>\\t<wav path relative to out-dir>\\t<reference>`, one line
per utterance. Tabs and newlines inside a reference are collapsed to spaces,
since one in a field would silently shift every column after it.

Two dataset shapes are understood:

* `answer` — AISHELL and friends: bare 汉字, no punctuation.
* `original_text` + `target_text` — Speechio: the same audio transcribed
  twice, once verbatim and once as punctuated written Chinese. Which one is
  written to the manifest is chosen by `--reference`, and the other is dropped;
  the manifest has one reference column because a scorer that did not know
  which it was reading would produce numbers nobody could compare.

The audio is written through unchanged. Whatever the parquet holds is what the
recognizer is given — resampling here would hide exactly the kind of problem
this is meant to measure.
"""

import argparse
import sys
from pathlib import Path

import pyarrow.parquet as pq


def escape(text: str) -> str:
    """Flatten text so it can sit in one tab-separated field."""
    return " ".join(text.split())


def pick_reference(row: dict, wanted: str) -> tuple[str, str] | None:
    """Return `(field name, text)` for the reference this dataset offers."""
    if wanted == "auto":
        for field in ("target_text", "original_text", "answer"):
            if field in row:
                return field, row[field]
        return None

    if wanted in row:
        return wanted, row[wanted]
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("parquet", type=Path)
    parser.add_argument("out_dir", type=Path)
    parser.add_argument(
        "--reference",
        default="auto",
        help="which transcript to score against (default: auto)",
    )
    args = parser.parse_args()

    wav_dir = args.out_dir / "wav"
    wav_dir.mkdir(parents=True, exist_ok=True)

    parquet = pq.ParquetFile(args.parquet)
    written = 0
    skipped = 0
    used: str | None = None

    with (args.out_dir / "manifest.tsv").open("w", encoding="utf-8", newline="\n") as manifest:
        for batch in parquet.iter_batches(batch_size=64):
            for row in batch.to_pylist():
                audio = row.get("audio") or row.get("context")
                if not audio or not audio.get("bytes"):
                    skipped += 1
                    continue

                picked = pick_reference(row, args.reference)
                if picked is None:
                    skipped += 1
                    continue
                field, text = picked
                used = used or field

                reference = escape(text)
                if not reference:
                    # Nothing to score against. Counting it as an empty
                    # reference would report a perfect score for audio that
                    # was never transcribed.
                    skipped += 1
                    continue

                name = f"{written:06d}"
                (wav_dir / f"{name}.wav").write_bytes(audio["bytes"])
                manifest.write(f"{name}\twav/{name}.wav\t{reference}\n")
                written += 1

    print(f"wrote {written} utterances to {args.out_dir} (skipped {skipped})")
    print(f"scored against: {used}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
