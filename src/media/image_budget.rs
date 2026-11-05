//! Cooperative image deadlines and bounded generated output.

use std::io::{self, Seek, SeekFrom, Write};
use std::time::{Duration, Instant};

/// A processing failure with an explicit fallback classification.
#[expect(
    clippy::redundant_pub_crate,
    reason = "image failure types cross into upload validation without becoming public API"
)]
#[derive(Debug, thiserror::Error)]
pub(crate) enum ImageFailure {
    /// A native processing or generated-output limit was reached.
    #[error("{0}")]
    Resource(&'static str),
    /// A decoded image or animation is structurally invalid.
    #[error("{0}")]
    Malformed(&'static str),
    /// Valid input cannot be represented by the output format.
    #[error("{0}")]
    Unsupported(&'static str),
}

/// One deadline spanning image validation, conversion and thumbnail work.
///
/// Checks run between library calls. They cannot interrupt a running decoder,
/// encoder or allocator, nor contain an aborting library panic.
#[expect(
    clippy::redundant_pub_crate,
    reason = "the opaque image budget crosses into upload validation without becoming public API"
)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct ImageBudget {
    /// When validation or an independent processing operation began.
    started: Instant,
    /// Deterministic clock used only by focused deadline regression checks.
    #[cfg(test)]
    elapsed: fn(Instant) -> Duration,
    /// Existing operator-configured native media timeout.
    timeout: Duration,
    /// Maximum generated artifact size; encoder-internal buffers are separate.
    pub(super) output_limit: u64,
}

impl ImageBudget {
    /// Start an operation with the shared output safety ceiling.
    pub(crate) fn new(output_limit: u64) -> Self {
        Self {
            started: Instant::now(),
            #[cfg(test)]
            elapsed: |started| started.elapsed(),
            timeout: Duration::from_secs(crate::config::ffmpeg_timeout_secs()),
            output_limit: output_limit.min(super::UNTRUSTED_IMAGE_DECODER_MAX_ALLOC_BYTES),
        }
    }

    /// Check a cooperative deadline before/after an expensive phase.
    pub(crate) fn check(&self) -> Result<(), ImageFailure> {
        #[cfg(test)]
        let elapsed = (self.elapsed)(self.started);
        #[cfg(not(test))]
        let elapsed = self.started.elapsed();
        if elapsed >= self.timeout {
            return Err(ImageFailure::Resource("image processing timed out"));
        }
        Ok(())
    }
}

impl Default for ImageBudget {
    fn default() -> Self {
        Self::new(super::UNTRUSTED_IMAGE_DECODER_MAX_ALLOC_BYTES)
    }
}

/// Limits bytes passed to an artifact writer, including animation frame spools.
pub(super) struct BudgetWriter<'a, W> {
    /// Destination owned or borrowed for this encoding phase.
    inner: W,
    /// Shared cooperative deadline.
    budget: &'a ImageBudget,
    /// Remaining bytes allowed through the writer.
    remaining: u64,
}

impl<'a, W> BudgetWriter<'a, W> {
    /// Construct a writer constrained by the artifact's remaining allowance.
    pub(super) const fn new(inner: W, limit: u64, budget: &'a ImageBudget) -> Self {
        Self {
            inner,
            budget,
            remaining: limit,
        }
    }
}

impl<W: Write> Write for BudgetWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.budget.check().map_err(io::Error::other)?;
        let length = u64::try_from(bytes.len()).map_err(io::Error::other)?;
        if length > self.remaining {
            return Err(io::Error::other(ImageFailure::Resource(
                "image output exceeds safety budget",
            )));
        }
        let written = self.inner.write(bytes)?;
        self.remaining = self
            .remaining
            .saturating_sub(u64::try_from(written).map_err(io::Error::other)?);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Seek> Seek for BudgetWriter<'_, W> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}

#[cfg(test)]
#[path = "tests/image_budget.rs"]
mod tests;
