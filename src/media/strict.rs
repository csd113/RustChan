//! Conservative opt-in sanitization: only freshly encoded pixels are published.

#![expect(
    clippy::redundant_pub_crate,
    reason = "sanitization helpers cross private subsystem boundaries without becoming public API"
)]

use super::{convert::ConversionResult, image_budget::ImageBudget, images};
use crate::utils::files::UploadRejection;
use anyhow::{Context as _, Result};
use image::ImageDecoder as _;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

/// Safe explanation shared by primary and secondary upload rejection paths.
pub(crate) const UNSUPPORTED_MESSAGE: &str = "This board requires metadata-safe uploads. Supported: JPEG, static PNG, BMP, GIF and WebP. Audio, video, PDF, SVG, HEIC/HEIF, TIFF and other files cannot currently be sanitized and are rejected.";

/// Reject formats whose complete presentation cannot be safely re-encoded here.
pub(crate) fn validate_format(input: &Path, mime: &str) -> Result<()> {
    match mime {
        "image/jpeg" | "image/bmp" | "image/gif" => Ok(()),
        "image/png" => {
            let decoder = image::codecs::png::PngDecoder::with_limits(
                BufReader::new(File::open(input)?),
                super::untrusted_image_decode_limits(),
            )?;
            if decoder.is_apng()? {
                anyhow::bail!(UploadRejection::Disallowed(
                    "This board requires metadata-safe uploads. APNG animation cannot currently be preserved; upload GIF or animated WebP instead.".into()
                ));
            }
            Ok(())
        }
        "image/webp" => {
            let mut decoder =
                image::codecs::webp::WebPDecoder::new(BufReader::new(File::open(input)?))?;
            decoder.set_limits(super::untrusted_image_decode_limits())?;
            // The existing compositor preserves timing/repetition but does not
            // transform every frame. Never silently drop animation orientation.
            if decoder.has_animation()
                && decoder.orientation()? != image::metadata::Orientation::NoTransforms
            {
                anyhow::bail!(UploadRejection::Disallowed(
                    "This board requires metadata-safe uploads. Oriented WebP animation cannot currently be preserved; rotate the frames before uploading.".into()
                ));
            }
            Ok(())
        }
        _ => Err(UploadRejection::Disallowed(UNSUPPORTED_MESSAGE.into()).into()),
    }
}

/// Re-encode every accepted image, irrespective of output size or source format.
/// Atomic staging and the shared byte/deadline budget prevent partial outputs;
/// codec errors never invoke the ordinary original-byte fallback.
pub(crate) fn sanitize(
    input: &Path,
    mime: &str,
    output_dir: &Path,
    file_stem: &str,
    budget: &ImageBudget,
) -> Result<ConversionResult> {
    budget.check()?;
    validate_format(input, mime)?;
    let output = output_dir.join(format!("{file_stem}.webp"));
    // Close the staging handle before atomic replacement on Windows.
    let temporary = tempfile::NamedTempFile::new_in(output_dir)?.into_temp_path();
    images::image_to_webp_with_budget(input, &temporary, None, budget).map_err(|error| {
        tracing::warn!(failure = images::failure_kind(&error), error = %error, "strict metadata sanitization rejected upload");
        if images::failure_kind(&error) == "io" {
            return error.context("strict metadata sanitizer filesystem operation failed");
        }
        error.context(UploadRejection::SanitizationFailed(
            "This board requires metadata-safe uploads. Sanitization failed or exceeded processing limits; the original file was not published. Try a smaller or re-exported image.".into(),
        ))
    })?;
    budget.check()?;
    let final_size = std::fs::metadata(&temporary)?.len();
    std::fs::rename(&temporary, &output).context("publish sanitized upload")?;
    Ok(ConversionResult {
        final_path: output,
        final_mime: "image/webp",
        was_converted: true,
        final_size,
    })
}

#[cfg(test)]
#[path = "tests/strict.rs"]
mod tests;
