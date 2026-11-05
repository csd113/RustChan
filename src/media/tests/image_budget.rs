//! Controlled deadline checks without slow tests or unbounded media.

use super::{BudgetWriter, ImageBudget};
use anyhow::{ensure, Result};
use std::io::Write as _;

/// Expired budgets must refuse writes and clean any temporary image output.
#[test]
fn expired_deadline_refuses_output() -> Result<()> {
    let budget = ImageBudget {
        started: std::time::Instant::now(),
        elapsed: |started| started.elapsed(),
        timeout: std::time::Duration::ZERO,
        output_limit: 1024,
    };
    let mut bytes = Vec::new();
    let result = BudgetWriter::new(&mut bytes, 1024, &budget).write_all(b"frame");
    ensure!(
        result.is_err() && bytes.is_empty(),
        "expired deadline wrote output"
    );
    let directory = tempfile::tempdir()?;
    let input = directory.path().join("input.png");
    let output = directory.path().join("output.webp");
    image::RgbaImage::new(1, 1).save(&input)?;
    ensure!(
        crate::media::images::image_to_webp_with_budget(&input, &output, None, &budget).is_err(),
        "expired image published"
    );
    ensure!(
        !output.exists() && std::fs::read_dir(directory.path())?.count() == 1,
        "expired image leaked output"
    );
    Ok(())
}

/// A codec can finish its last write after the deadline without another writer
/// call. A controlled clock protects the post-encode check without sleeping.
#[test]
fn deadline_expiring_during_final_codec_write_is_rejected() -> Result<()> {
    use std::cell::Cell;
    use std::io::{self, Cursor, Seek, SeekFrom, Write};
    use std::time::{Duration, Instant};
    thread_local! { static EXPIRED: Cell<bool> = const { Cell::new(false) }; }
    /// Writer that advances the test clock only after the final encoded byte.
    struct FinalWrite {
        /// Encoded bytes and the codec's seek cursor.
        bytes: Cursor<Vec<u8>>,
        /// Previously measured complete output length for this tiny image.
        expected: usize,
    }
    impl Write for FinalWrite {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let count = self.bytes.write(bytes)?;
            if self.bytes.get_ref().len() == self.expected {
                EXPIRED.set(true);
            }
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Seek for FinalWrite {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.bytes.seek(position)
        }
    }
    let pixels = image::DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2));
    let mut reference = Cursor::new(Vec::new());
    crate::media::images::write_pixels(&pixels, &mut reference, &ImageBudget::default())?;
    let budget = ImageBudget {
        started: Instant::now(),
        timeout: Duration::from_secs(1),
        output_limit: 1024,
        elapsed: |_started| {
            if EXPIRED.get() {
                Duration::from_secs(1)
            } else {
                Duration::ZERO
            }
        },
    };
    let mut writer = FinalWrite {
        bytes: Cursor::new(Vec::new()),
        expected: reference.get_ref().len(),
    };
    EXPIRED.set(false);
    let result = crate::media::images::write_pixels(&pixels, &mut writer, &budget);
    ensure!(
        EXPIRED.get() && writer.bytes.into_inner() == reference.into_inner(),
        "codec did not finish its last write"
    );
    ensure!(
        result.is_err(),
        "last codec write overran deadline without rejection"
    );
    Ok(())
}
