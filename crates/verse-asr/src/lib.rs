//! Speech recognition engine implementations.
//!
//! This is the only crate that links sherpa-onnx, and therefore the only one
//! whose build needs the native archive — see `design.md` §7.2. The traits it
//! implements live in `verse-core::traits`, so nothing above this crate depends
//! on the FFI.
//!
//! Implementations land in P1a step 7. The traits are `AudioSource`,
//! `AsrEngine` and `TextSink` in `verse_core::traits`.

#[cfg(test)]
mod tests {
    use sherpa_onnx::LinearResampler;

    /// Building an empty crate never exercises the linker, so this test exists
    /// to prove the native sherpa-onnx library is genuinely linked and callable
    /// rather than merely compiled.
    ///
    /// It uses the resampler because that needs no model files. If the archive
    /// is missing or the link flags are wrong, this fails to link rather than
    /// failing at runtime.
    #[test]
    fn native_library_is_linked_and_callable() {
        let resampler =
            LinearResampler::create(48_000, 16_000).expect("resampler should be constructible");

        assert_eq!(resampler.input_sample_rate(), 48_000);
        assert_eq!(resampler.output_sample_rate(), 16_000);

        // 0.1 s of audio at 48 kHz should come back as roughly 0.1 s at 16 kHz.
        // A range rather than an exact count: the filter has edge behaviour and
        // the library does not promise a precise sample count.
        let input = vec![0.0f32; 4_800];
        let output = resampler.resample(&input, true);
        assert!(
            (1_500..=1_700).contains(&output.len()),
            "expected roughly 1600 samples after downsampling, got {}",
            output.len()
        );
    }
}
