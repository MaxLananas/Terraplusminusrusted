//! Times the startup decode of the bundled Köppen raster (inflate, CRC-32, run-length build).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::time::Instant;
fn main() {
    let gz = terra_chunk_generation::KoppenMap::embedded_bytes();
    let t = Instant::now();
    let n = terra_codec::gunzip_streaming(gz, usize::MAX, &mut |_b: &[u8]| Ok(())).unwrap();
    println!("gunzip_streaming empty sink {:?} ({n} bytes)", t.elapsed());
    let t = Instant::now();
    let (n, _) = terra_codec::inflate_streaming(&gz[10..], usize::MAX, &mut |_b: &[u8]| Ok(())).unwrap();
    println!("inflate only {:?} ({n})", t.elapsed());
    let buf = vec![7u8; 64 << 20];
    let t = Instant::now();
    std::hint::black_box(terra_codec::crc32(&buf));
    println!("crc32 64MiB {:?}", t.elapsed());
    let t = Instant::now();
    let k = terra_chunk_generation::KoppenMap::from_gzip(gz).unwrap();
    println!("koppen from_gzip {:?} runs={}", t.elapsed(), k.runs());
}
