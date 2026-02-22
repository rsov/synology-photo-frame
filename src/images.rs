extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use embedded_graphics::pixelcolor::Rgb888;
use epd_waveshare::prelude::HexColor;

// This bit was Ai generated
// The resize crate used too much memory and needed too much ceremony to covnert between RGB structs
pub fn mitchell_upscale(
    src: Vec<u8>,
    src_width: usize,
    src_height: usize,
    target_width: usize,
    target_height: usize,
) -> (Vec<u8>, usize, usize) {
    // Calculate aspect ratios to fit within target dimensions
    let src_aspect = src_width as f32 / src_height as f32;
    let target_aspect = target_width as f32 / target_height as f32;

    let (new_width, new_height) = if src_aspect > target_aspect {
        // Fit by width
        (target_width, (target_width as f32 / src_aspect) as usize)
    } else {
        // Fit by height
        ((target_height as f32 * src_aspect) as usize, target_height)
    };

    let mut output = vec![0u8; new_height * new_width * 3];

    let x_ratio = src_width as f32 / new_width as f32;
    let y_ratio = src_height as f32 / new_height as f32;

    // Bilinear interpolation (simpler than full Mitchell for embedded)
    for y in 0..new_height {
        for x in 0..new_width {
            let src_x = x as f32 * x_ratio;
            let src_y = y as f32 * y_ratio;

            let x0 = src_x as usize;
            let y0 = src_y as usize;
            let x1 = (x0 + 1).min(src_width - 1);
            let y1 = (y0 + 1).min(src_height - 1);

            let fx = src_x - x0 as f32;
            let fy = src_y - y0 as f32;

            let out_idx = (y * new_width + x) * 3;

            for c in 0..3 {
                let p00 = src[(y0 * src_width + x0) * 3 + c] as f32;
                let p10 = src[(y0 * src_width + x1) * 3 + c] as f32;
                let p01 = src[(y1 * src_width + x0) * 3 + c] as f32;
                let p11 = src[(y1 * src_width + x1) * 3 + c] as f32;

                let interpolated = p00 * (1.0 - fx) * (1.0 - fy)
                    + p10 * fx * (1.0 - fy)
                    + p01 * (1.0 - fx) * fy
                    + p11 * fx * fy;

                output[out_idx + c] = interpolated.clamp(0.0, 255.0) as u8;
            }
        }
    }

    (output, new_width, new_height)
}

// This bit was Ai generated. Could implement better buffer handling
pub fn floyd_steinberg_dither(width: usize, src: Vec<u8>) -> Vec<u8> {
    let height = src.len() / (width * 3);
    assert_eq!(src.len(), width * height * 3);

    let mut work: Vec<[f32; 3]> = src
        .chunks_exact(3)
        .map(|c| [c[0] as f32, c[1] as f32, c[2] as f32])
        .collect();

    let mut out = vec![0u8; width * height];

    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;

            let old = work[idx];

            // Convert to Rgb888 for your existing From<Rgb888> → HexColor
            let rgb = Rgb888::new(
                old[0].clamp(0.0, 255.0) as u8,
                old[1].clamp(0.0, 255.0) as u8,
                old[2].clamp(0.0, 255.0) as u8,
            );

            let new_color = HexColor::from(rgb);
            out[idx] = new_color.get_nibble();

            // Quantized RGB from your palette
            let (qr, qg, qb) = new_color.rgb();
            let quant = [qr as f32, qg as f32, qb as f32];

            // Error = original - quantized
            let err = [old[0] - quant[0], old[1] - quant[1], old[2] - quant[2]];

            // Floyd–Steinberg diffusion
            //       *   7/16
            //  3/16 5/16 1/16

            // Right
            if x + 1 < width {
                let i = idx + 1;
                for c in 0..3 {
                    work[i][c] += err[c] * 7.0 / 16.0;
                }
            }

            // Bottom row
            if y + 1 < height {
                // Bottom-left
                if x > 0 {
                    let i = idx + width - 1;
                    for c in 0..3 {
                        work[i][c] += err[c] * 3.0 / 16.0;
                    }
                }

                // Bottom
                {
                    let i = idx + width;
                    for c in 0..3 {
                        work[i][c] += err[c] * 5.0 / 16.0;
                    }
                }

                // Bottom-right
                if x + 1 < width {
                    let i = idx + width + 1;
                    for c in 0..3 {
                        work[i][c] += err[c] * 1.0 / 16.0;
                    }
                }
            }
        }
    }

    out
}
