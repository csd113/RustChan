//! Cross-platform publication regression for the PNG-if-smaller path.

use anyhow::{ensure, Result};

/// A successful smaller WebP must publish rather than silently retain the PNG.
#[test]
fn smaller_png_conversion_publishes_webp_and_cleans_staging() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let input = directory.path().join("input.png");
    let source = image::RgbImage::from_pixel(16, 16, image::Rgb([20, 80, 160]));
    source.save(&input)?;
    let result = super::convert_file(&input, "image/png", directory.path(), "converted")?;
    ensure!(
        result.was_converted && result.final_mime == "image/webp",
        "PNG conversion silently fell back"
    );
    ensure!(
        result.final_size < std::fs::metadata(&input)?.len(),
        "PNG conversion did not reduce size"
    );
    ensure!(
        image::open(result.final_path)?.into_rgb8() == source,
        "PNG pixels changed"
    );
    ensure!(
        std::fs::read_dir(directory.path())?.count() == 2,
        "PNG staging output leaked"
    );
    Ok(())
}
