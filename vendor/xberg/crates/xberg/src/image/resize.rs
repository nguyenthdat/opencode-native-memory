use crate::error::{Result, XbergError};
use fast_image_resize::{
    FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer,
    images::{Image as FirImage, ImageRef as FirImageRef},
};

/// Resize RGB pixels using `fast_image_resize` without copying the source buffer.
pub(crate) fn resize_rgb(
    rgb_data: &[u8],
    width: u32,
    height: u32,
    new_width: u32,
    new_height: u32,
    scale_factor: f64,
) -> Result<Vec<u8>> {
    let src_image = FirImageRef::new(width, height, rgb_data, PixelType::U8x3)
        .map_err(|e| XbergError::parsing(format!("Failed to create source image: {e:?}")))?;

    let mut dst_image = FirImage::new(new_width, new_height, PixelType::U8x3);

    let algorithm = if scale_factor < 1.0 {
        ResizeAlg::Convolution(FilterType::Lanczos3)
    } else {
        ResizeAlg::Convolution(FilterType::CatmullRom)
    };

    let mut resizer = Resizer::new();
    resizer
        .resize(&src_image, &mut dst_image, &ResizeOptions::new().resize_alg(algorithm))
        .map_err(|e| XbergError::parsing(format!("Resize failed: {e:?}")))?;

    Ok(dst_image.into_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, Rgb};

    fn create_test_rgb_data(width: u32, height: u32) -> Vec<u8> {
        let mut image = ImageBuffer::new(width, height);
        for y in 0..height {
            for x in 0..width {
                image.put_pixel(
                    x,
                    y,
                    Rgb([
                        ((x * 31 + y * 17) % 256) as u8,
                        ((x * 13 + y * 47) % 256) as u8,
                        ((x * 73 + y * 7) % 256) as u8,
                    ]),
                );
            }
        }
        image.into_raw()
    }

    fn reference_resize(
        rgb_data: &[u8],
        width: u32,
        height: u32,
        new_width: u32,
        new_height: u32,
        scale_factor: f64,
    ) -> Vec<u8> {
        let image_buffer = ImageBuffer::<Rgb<u8>, Vec<u8>>::from_raw(width, height, rgb_data.to_vec())
            .expect("reference input dimensions must match its buffer");
        let image = DynamicImage::ImageRgb8(image_buffer);
        let rgb_image = image.to_rgb8();
        let source = FirImage::from_vec_u8(width, height, rgb_image.into_raw(), PixelType::U8x3)
            .expect("reference source image must be valid");
        let mut destination = FirImage::new(new_width, new_height, PixelType::U8x3);
        let algorithm = if scale_factor < 1.0 {
            ResizeAlg::Convolution(FilterType::Lanczos3)
        } else {
            ResizeAlg::Convolution(FilterType::CatmullRom)
        };
        Resizer::new()
            .resize(&source, &mut destination, &ResizeOptions::new().resize_alg(algorithm))
            .expect("reference resize must succeed");
        let resized_buffer = ImageBuffer::<Rgb<u8>, Vec<u8>>::from_raw(new_width, new_height, destination.into_vec())
            .expect("reference destination dimensions must match its buffer");
        DynamicImage::ImageRgb8(resized_buffer).to_rgb8().into_raw()
    }

    #[test]
    fn should_match_reference_pixels_when_downscaling() {
        let width = 17;
        let height = 11;
        let rgb_data = create_test_rgb_data(width, height);
        let expected = reference_resize(&rgb_data, width, height, 7, 5, 0.45);

        let actual = resize_rgb(&rgb_data, width, height, 7, 5, 0.45).expect("optimized downscale should succeed");

        assert_eq!(actual, expected, "optimized downscale changed output pixels");
    }

    #[test]
    fn should_match_reference_pixels_when_upscaling() {
        let width = 9;
        let height = 6;
        let rgb_data = create_test_rgb_data(width, height);
        let expected = reference_resize(&rgb_data, width, height, 23, 15, 2.5);

        let actual = resize_rgb(&rgb_data, width, height, 23, 15, 2.5).expect("optimized upscale should succeed");

        assert_eq!(actual, expected, "optimized upscale changed output pixels");
    }

    #[test]
    fn should_match_reference_pixels_at_unit_scale() {
        let width = 8;
        let height = 5;
        let rgb_data = create_test_rgb_data(width, height);
        let expected = reference_resize(&rgb_data, width, height, width, height, 1.0);

        let actual =
            resize_rgb(&rgb_data, width, height, width, height, 1.0).expect("unit-scale resize should succeed");

        assert_eq!(actual, expected, "optimized unit-scale resize changed output pixels");
    }

    #[test]
    fn should_not_modify_source_pixels() {
        let rgb_data = create_test_rgb_data(11, 7);
        let original = rgb_data.clone();

        let _ = resize_rgb(&rgb_data, 11, 7, 19, 12, 1.75).expect("resize should succeed");

        assert_eq!(rgb_data, original, "borrowed source buffer was modified");
    }

    #[test]
    fn should_reject_source_buffer_with_invalid_size() {
        let rgb_data = vec![0; 11];

        let error = resize_rgb(&rgb_data, 2, 2, 4, 4, 2.0).expect_err("invalid source buffer must fail");

        assert!(
            error.to_string().contains("Failed to create source image"),
            "unexpected error: {error}"
        );
    }
}
