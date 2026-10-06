//! Passing audio on without deciding anything about it.
//!
//! The fallback for a file the detector has disbelieved. When the segmenter
//! reports almost no speech in a file that plainly contains some, the audio is
//! handed to the recogniser whole rather than not at all — `tasks/` records
//! what that costs when it is skipped.
//!
//! It still cuts, at a fixed interval, and that is deliberate. Holding an
//! entire four-hour recording resident is the failure mode segmentation exists
//! to prevent; a cut every [`MAX_SPAN_SECONDS`] keeps the same ceiling while
//! losing nothing. On the files this actually fires for — a few seconds long —
//! there is no cut at all, because the whole file is one block.

use std::time::Duration;

use verse_core::{AudioChunk, AudioFormat, Result, Segmenter};

use crate::vad::MAX_SPAN_SECONDS;

/// A segmenter that keeps everything, in fixed-length blocks.
pub struct FixedBlocks {
    format: AudioFormat,
    block_samples: usize,
    /// Audio accepted but not yet emitted.
    pending: Vec<f32>,
    /// Samples emitted so far, which is where the next block starts.
    emitted: usize,
}

impl FixedBlocks {
    pub fn new(format: AudioFormat) -> Self {
        let rate = format.sample_rate.max(1) as f64;
        Self {
            format,
            block_samples: (MAX_SPAN_SECONDS as f64 * rate) as usize,
            pending: Vec::new(),
            emitted: 0,
        }
    }

    fn take_block(&mut self, samples: Vec<f32>) -> AudioChunk {
        let rate = self.format.sample_rate.max(1) as f64;
        let chunk = AudioChunk {
            samples,
            format: self.format,
            start: Duration::from_secs_f64(self.emitted as f64 / rate),
        };
        self.emitted += chunk.samples.len();
        chunk
    }

    fn drain_blocks(&mut self) -> Vec<AudioChunk> {
        let mut blocks = Vec::new();
        while self.pending.len() >= self.block_samples {
            let samples: Vec<f32> = self.pending.drain(..self.block_samples).collect();
            blocks.push(self.take_block(samples));
        }
        blocks
    }
}

impl Segmenter for FixedBlocks {
    fn accept(&mut self, chunk: &AudioChunk) -> Result<()> {
        self.pending.extend_from_slice(&chunk.samples);
        Ok(())
    }

    fn take(&mut self) -> Vec<AudioChunk> {
        self.drain_blocks()
    }

    fn finish(&mut self) -> Vec<AudioChunk> {
        let mut blocks = self.drain_blocks();
        if !self.pending.is_empty() {
            let samples = std::mem::take(&mut self.pending);
            blocks.push(self.take_block(samples));
        }
        blocks
    }

    fn reset(&mut self) {
        self.pending.clear();
        self.emitted = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn format() -> AudioFormat {
        AudioFormat {
            sample_rate: RATE,
            channels: 1,
        }
    }

    fn feed(seconds: f64) -> Vec<AudioChunk> {
        let mut segmenter = FixedBlocks::new(format());
        let samples = vec![0.1f32; (seconds * RATE as f64) as usize];
        segmenter
            .accept(&AudioChunk {
                samples,
                format: format(),
                start: Duration::ZERO,
            })
            .unwrap();
        segmenter.finish()
    }

    #[test]
    fn a_short_file_is_one_block() {
        let blocks = feed(5.0);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].samples.len(), 5 * RATE as usize);
        assert_eq!(blocks[0].start, Duration::ZERO);
    }

    #[test]
    fn a_long_file_is_cut_at_the_memory_bound() {
        let blocks = feed(50.0);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].samples.len(), MAX_SPAN_SECONDS as usize * RATE as usize);
        assert_eq!(blocks[2].samples.len(), 10 * RATE as usize);
    }

    #[test]
    fn nothing_is_dropped_whatever_the_length() {
        for seconds in [0.5, 5.0, 20.0, 20.5, 61.0] {
            let total: usize = feed(seconds).iter().map(|b| b.samples.len()).sum();
            assert_eq!(total, (seconds * RATE as f64) as usize, "{seconds}s");
        }
    }

    #[test]
    fn blocks_are_contiguous_in_time() {
        let blocks = feed(45.0);
        let mut expected = Duration::ZERO;
        for block in &blocks {
            assert_eq!(block.start, expected);
            expected += block.duration();
        }
    }
}
