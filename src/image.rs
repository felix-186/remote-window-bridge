use crate::error::{DesktopError, Result};
use flate2::{write::ZlibEncoder, Compression};
use std::io::Write;

/// Tight RGB8 rows, top to bottom. Platform-specific BGRA never crosses this boundary.
#[derive(Debug, Clone)]
pub struct RgbImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}
impl RgbImage {
    pub fn validate(&self) -> Result<()> {
        let size = (self.width as u64)
            .checked_mul(self.height as u64)
            .and_then(|n| n.checked_mul(3))
            .ok_or_else(|| DesktopError::new("INVALID_FRAME", "RGB dimensions overflow"))?;
        if self.width == 0
            || self.height == 0
            || size > 64 * 1024 * 1024
            || size != self.pixels.len() as u64
        {
            return Err(DesktopError::new(
                "INVALID_FRAME",
                "Invalid RGB buffer or frame exceeds 64 MiB",
            ));
        }
        Ok(())
    }
    pub fn png(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut zlib = ZlibEncoder::new(Vec::new(), Compression::fast());
        for row in self.pixels.chunks_exact(self.width as usize * 3) {
            zlib.write_all(&[0])
                .and_then(|_| zlib.write_all(row))
                .map_err(image_error)?;
        }
        let compressed = zlib.finish().map_err(image_error)?;
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut header = Vec::new();
        header.extend(self.width.to_be_bytes());
        header.extend(self.height.to_be_bytes());
        header.extend([8, 2, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &header);
        chunk(&mut out, b"IDAT", &compressed);
        chunk(&mut out, b"IEND", &[]);
        Ok(out)
    }
    /// Fraction of pixels with a channel difference > 30. A signal, not a semantic verifier.
    pub fn difference(&self, other: &Self) -> f64 {
        if self.width != other.width || self.height != other.height {
            return 1.0;
        }
        let changed = self
            .pixels
            .chunks_exact(3)
            .zip(other.pixels.chunks_exact(3))
            .filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 30))
            .count();
        changed as f64 / (self.width as f64 * self.height as f64).max(1.0)
    }

    /// Inspect a clipped 96x96 neighborhood without diluting a small target into
    /// the whole viewport. A visual guard, not a semantic correctness check.
    pub fn difference_near(&self, other: &Self, x: i64, y: i64) -> f64 {
        if self.width != other.width || self.height != other.height {
            return 1.0;
        }
        let left = (x - 48).clamp(0, self.width as i64) as usize;
        let right = (x + 48).clamp(0, self.width as i64) as usize;
        let top = (y - 48).clamp(0, self.height as i64) as usize;
        let bottom = (y + 48).clamp(0, self.height as i64) as usize;
        let mut changed = 0usize;
        for row in top..bottom {
            for col in left..right {
                let offset = (row * self.width as usize + col) * 3;
                if (0..3).any(|c| self.pixels[offset + c].abs_diff(other.pixels[offset + c]) > 30) {
                    changed += 1;
                }
            }
        }
        changed as f64 / ((right - left) * (bottom - top)).max(1) as f64
    }
}
fn image_error(e: std::io::Error) -> DesktopError {
    DesktopError::new("IMAGE_ENCODING", e.to_string())
}
fn chunk(out: &mut Vec<u8>, name: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    out.extend(name);
    out.extend(data);
    let mut crc = crc32fast::Hasher::new();
    crc.update(name);
    crc.update(data);
    out.extend(crc.finalize().to_be_bytes());
}
