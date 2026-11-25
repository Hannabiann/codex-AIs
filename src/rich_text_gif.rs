//! RTF has no GIF picture type. Encode its first displayed frame as RGBA PNG.
//! Keep transparency and the logical-screen offset; no platform codecs needed.
use std::{io::Write, num::NonZeroU64};

const MAX_PIXELS_BYTES: u64 = 64 * 1024 * 1024;

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = crc32fast::Hasher::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.finalize().to_be_bytes());
}

pub(super) fn png(data: &[u8]) -> Result<(Vec<u8>, u16, u16), String> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    options.set_memory_limit(gif::MemoryLimit::Bytes(
        NonZeroU64::new(MAX_PIXELS_BYTES).unwrap(),
    ));
    options.check_frame_consistency(true);
    options.check_lzw_end_code(true);
    let mut decoder = options
        .read_info(data)
        .map_err(|e| format!("RTF GIF header: {e}"))?;
    let (width, height) = (decoder.width(), decoder.height());
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) * 4 > MAX_PIXELS_BYTES {
        return Err("RTF GIF canvas size limit".into());
    }
    let background = decoder
        .bg_color()
        .and_then(|index| decoder.global_palette()?.get(index * 3..index * 3 + 3))
        .map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
        .unwrap_or([0; 4]);
    let frame = decoder
        .read_next_frame()
        .map_err(|e| format!("RTF GIF frame: {e}"))?
        .ok_or("RTF GIF has no frame")?;
    let background = if frame.transparent.is_some() {
        [0; 4]
    } else {
        background
    };
    let mut row = vec![0; 1 + width as usize * 4];
    let mut compressed =
        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    // Stream canvas rows instead of allocating a second full RGBA image.
    for y in 0..height as usize {
        for pixel in row[1..].chunks_exact_mut(4) {
            pixel.copy_from_slice(&background);
        }
        if (frame.top as usize..frame.top as usize + frame.height as usize).contains(&y) {
            let begin = (y - frame.top as usize) * frame.width as usize * 4;
            let left = 1 + frame.left as usize * 4;
            row[left..left + frame.width as usize * 4]
                .copy_from_slice(&frame.buffer[begin..begin + frame.width as usize * 4]);
        }
        compressed
            .write_all(&row)
            .map_err(|e| format!("RTF PNG encode: {e}"))?;
    }
    let compressed = compressed
        .finish()
        .map_err(|e| format!("RTF PNG encode: {e}"))?;
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&(width as u32).to_be_bytes());
    header.extend_from_slice(&(height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &compressed);
    chunk(&mut out, b"IEND", &[]);
    Ok((out, width, height))
}
