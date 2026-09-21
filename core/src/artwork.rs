//! Artwork fetching and normalization.
//!
//! Downloads poster images, decodes them, and can re-encode to JPEG/PNG for the
//! `covr` atom, or produce small RGBA thumbnails for the GUI poster grid.

use crate::error::Result;
use image::ImageFormat;

/// A decoded image ready to be written to an MP4 or displayed.
#[derive(Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// Raw RGBA8 pixels, row-major. Convenient for egui `ColorImage`.
    pub rgba: Vec<u8>,
}

/// The encoded artwork bytes plus their format, for the `covr` atom.
#[derive(Clone)]
pub struct EncodedArtwork {
    pub format: ArtworkFormat,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtworkFormat {
    Jpeg,
    Png,
}

/// Download raw image bytes from a URL.
pub async fn download(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let resp = client.get(url).send().await?.error_for_status()?;
    Ok(resp.bytes().await?.to_vec())
}

/// Decode encoded image bytes into RGBA for display.
pub fn decode(bytes: &[u8]) -> Result<DecodedImage> {
    let img = image::load_from_memory(bytes)?;
    let rgba = img.to_rgba8();
    Ok(DecodedImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// Produce a thumbnail (max dimension `max_side`) as RGBA for the GUI grid.
pub fn thumbnail(bytes: &[u8], max_side: u32) -> Result<DecodedImage> {
    let img = image::load_from_memory(bytes)?;
    let thumb = img.thumbnail(max_side, max_side).to_rgba8();
    Ok(DecodedImage {
        width: thumb.width(),
        height: thumb.height(),
        rgba: thumb.into_raw(),
    })
}

/// Read the original pixel dimensions of an encoded image without doing a full
/// RGBA conversion.
pub fn dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    Ok(reader.into_dimensions()?)
}

/// Maximum cover-art dimension (width or height) in pixels. Larger images are
/// downscaled to fit within a `MAX_COVER_DIMENSION` × `MAX_COVER_DIMENSION` box
/// (keeping aspect ratio) so the embedded `covr` stays small.
pub const MAX_COVER_DIMENSION: u32 = 1000;

/// Normalize downloaded artwork bytes into a format accepted by the `covr`
/// atom, clamping both width and height to [`MAX_COVER_DIMENSION`] (aspect
/// ratio preserved).
///
/// - If the image is within the size limit and already JPEG or PNG, its
///   bytes are passed through unchanged.
/// - Otherwise it is downscaled as needed and re-encoded to JPEG.
pub fn normalize_for_cover(bytes: &[u8]) -> Result<EncodedArtwork> {
    let format = image::guess_format(bytes).ok();
    let within_limit = match dimensions(bytes) {
        Ok((w, h)) => w <= MAX_COVER_DIMENSION && h <= MAX_COVER_DIMENSION,
        // If we can't read dimensions, fall through to a decode/re-encode.
        Err(_) => false,
    };

    if within_limit {
        match format {
            Some(ImageFormat::Jpeg) => {
                return Ok(EncodedArtwork {
                    format: ArtworkFormat::Jpeg,
                    bytes: bytes.to_vec(),
                })
            }
            Some(ImageFormat::Png) => {
                return Ok(EncodedArtwork {
                    format: ArtworkFormat::Png,
                    bytes: bytes.to_vec(),
                })
            }
            _ => {}
        }
    }

    // Needs downscaling and/or re-encoding.
    let img = image::load_from_memory(bytes)?;
    let scaled = if img.width() > MAX_COVER_DIMENSION || img.height() > MAX_COVER_DIMENSION {
        // `resize` fits the image *within* the given box, preserving aspect
        // ratio — so bounding both to MAX_COVER_DIMENSION clamps the larger
        // dimension and scales the other proportionally.
        img.resize(
            MAX_COVER_DIMENSION,
            MAX_COVER_DIMENSION,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        img
    };
    let mut out = std::io::Cursor::new(Vec::new());
    scaled.write_to(&mut out, ImageFormat::Jpeg)?;
    Ok(EncodedArtwork {
        format: ArtworkFormat::Jpeg,
        bytes: out.into_inner(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a solid RGBA image of the given size as PNG bytes.
    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn clamps_tall_image_to_max_dimension() {
        // 800 x 2000 -> height is the binding dimension.
        let (w, h) = dimensions(&normalize_for_cover(&png(800, 2000)).unwrap().bytes).unwrap();
        assert!(
            w <= MAX_COVER_DIMENSION && h <= MAX_COVER_DIMENSION,
            "{w}x{h}"
        );
        assert_eq!(h, MAX_COVER_DIMENSION); // scaled to fit height
        assert_eq!(w, 400); // aspect ratio preserved (800*1000/2000)
    }

    #[test]
    fn clamps_wide_image_to_max_dimension() {
        // 2000 x 800 -> width is the binding dimension (previously NOT clamped).
        let (w, h) = dimensions(&normalize_for_cover(&png(2000, 800)).unwrap().bytes).unwrap();
        assert!(
            w <= MAX_COVER_DIMENSION && h <= MAX_COVER_DIMENSION,
            "{w}x{h}"
        );
        assert_eq!(w, MAX_COVER_DIMENSION); // scaled to fit width
        assert_eq!(h, 400); // aspect ratio preserved (800*1000/2000)
    }

    #[test]
    fn passes_through_small_image() {
        // Within limits: dimensions unchanged.
        let (w, h) = dimensions(&normalize_for_cover(&png(600, 900)).unwrap().bytes).unwrap();
        assert_eq!((w, h), (600, 900));
    }
}
