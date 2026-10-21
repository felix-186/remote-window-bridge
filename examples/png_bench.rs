//! cargo run --release --example png_bench
//! Synthetic RGB workloads; compares the previous encoder with the buffered one.
use flate2::{write::ZlibEncoder, Compression};
use remote_window_bridge::image::RgbImage;
use std::{
    hint::black_box,
    io::Write,
    time::{Duration, Instant},
};

fn previous(image: &RgbImage) -> Vec<u8> {
    image.validate().unwrap();
    let mut zlib = ZlibEncoder::new(Vec::new(), Compression::fast());
    for row in image.pixels.chunks_exact(image.width as usize * 3) {
        zlib.write_all(&[0]).unwrap();
        zlib.write_all(row).unwrap();
    }
    let compressed = zlib.finish().unwrap();
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend(image.width.to_be_bytes());
    header.extend(image.height.to_be_bytes());
    header.extend([8, 2, 0, 0, 0]);
    for (name, data) in [
        (b"IHDR", header.as_slice()),
        (b"IDAT", compressed.as_slice()),
        (b"IEND", &[][..]),
    ] {
        out.extend((data.len() as u32).to_be_bytes());
        out.extend(name);
        out.extend(data);
        let mut crc = crc32fast::Hasher::new();
        crc.update(name);
        crc.update(data);
        out.extend(crc.finalize().to_be_bytes());
    }
    out
}
fn main() {
    for (width, height) in [(1280u32, 720u32), (1920, 1080)] {
        for noise in [false, true] {
            let mut pixels = vec![0; width as usize * height as usize * 3];
            let mut random = 0x12345678u32;
            for (i, pixel) in pixels.chunks_exact_mut(3).enumerate() {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                let x = i as u32 % width;
                let y = i as u32 / width;
                let color = if noise {
                    [random as u8, (random >> 8) as u8, (random >> 16) as u8]
                } else {
                    [((x / 16) % 8 * 30) as u8, ((y / 24) % 8 * 30) as u8, 220]
                };
                pixel.copy_from_slice(&color);
            }
            let image = RgbImage {
                width,
                height,
                pixels,
            };
            black_box(previous(&image));
            black_box(image.png().unwrap());
            let mut old = Duration::ZERO;
            let mut new = Duration::ZERO;
            for _ in 0..12 {
                let start = Instant::now();
                black_box(previous(black_box(&image)));
                old += start.elapsed();
                let start = Instant::now();
                black_box(image.png().unwrap());
                new += start.elapsed();
            }
            println!("{width}x{height} {}: previous {:.2} ms, buffered {:.2} ms, {:.1}% faster; PNG {} bytes",
                if noise { "noise" } else { "desktop-like" }, old.as_secs_f64() * 1000.0 / 12.0,
                new.as_secs_f64() * 1000.0 / 12.0, (1.0 - new.as_secs_f64() / old.as_secs_f64()) * 100.0,
                image.png().unwrap().len());
        }
    }
}
