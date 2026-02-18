extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use embedded_graphics::pixelcolor::Rgb888;
use epd_waveshare::prelude::HexColor;

// This bit was Ai generated. Could implement better buffer handling
pub fn floyd_steinberg_dither(width: usize, src: &[u8]) -> Vec<u8> {
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
