//! Streaming ZIP64 writer for daily log archives; UTF-8 names and raw deflate.
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
};
fn u16w(w: &mut File, n: u16) -> std::io::Result<()> {
    w.write_all(&n.to_le_bytes())
}
fn u32w(w: &mut File, n: u32) -> std::io::Result<()> {
    w.write_all(&n.to_le_bytes())
}
fn u64w(w: &mut File, n: u64) -> std::io::Result<()> {
    w.write_all(&n.to_le_bytes())
}
pub(super) fn write(output: &Path, root: &Path, paths: &[PathBuf]) -> std::io::Result<()> {
    let mut w = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)?;
    let mut central = Vec::new();
    for path in paths {
        let name = path
            .strip_prefix(root)
            .map_err(std::io::Error::other)?
            .to_string_lossy()
            .replace('\\', "/")
            .into_bytes();
        let name_len = u16::try_from(name.len()).map_err(std::io::Error::other)?;
        let modified = path
            .metadata()?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as f64;
        let date = crate::ks::date::Date::from_number(
            modified
                - crate::ks::date::Date::offset().map_err(std::io::Error::other)? as f64 * 60000.0,
        )
        .map_err(std::io::Error::other)?;
        let [year, month, day, hour, minute, second, ..] = date.parts();
        let dos_time = ((hour as u16) << 11) | ((minute as u16) << 5) | (second as u16 / 2);
        let dos_date = (((year.clamp(1980, 2107) - 1980) as u16) << 9)
            | (((month + 1) as u16) << 5)
            | day as u16;
        let offset = w.stream_position()?;
        u32w(&mut w, 0x04034b50)?;
        u16w(&mut w, 45)?;
        u16w(&mut w, 0x808)?;
        u16w(&mut w, 8)?;
        u16w(&mut w, dos_time)?;
        u16w(&mut w, dos_date)?;
        u32w(&mut w, 0)?;
        u32w(&mut w, u32::MAX)?;
        u32w(&mut w, u32::MAX)?;
        u16w(&mut w, name_len)?;
        u16w(&mut w, 20)?;
        w.write_all(&name)?;
        u16w(&mut w, 1)?;
        u16w(&mut w, 16)?;
        u64w(&mut w, 0)?;
        u64w(&mut w, 0)?;
        let start = w.stream_position()?;
        let mut input = File::open(path)?;
        let mut crc = crc32fast::Hasher::new();
        let mut size = 0u64;
        let mut buffer = [0u8; 65536];
        let mut deflate = flate2::write::DeflateEncoder::new(&mut w, flate2::Compression::fast());
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            crc.update(&buffer[..n]);
            deflate.write_all(&buffer[..n])?;
            size += n as u64;
        }
        deflate.finish()?;
        let packed = w.stream_position()? - start;
        let crc = crc.finalize();
        u32w(&mut w, 0x08074b50)?;
        u32w(&mut w, crc)?;
        u64w(&mut w, packed)?;
        u64w(&mut w, size)?;
        central.push((name, offset, crc, packed, size, dos_time, dos_date));
    }
    let start = w.stream_position()?;
    for (name, offset, crc, packed, size, dos_time, dos_date) in &central {
        u32w(&mut w, 0x02014b50)?;
        u16w(&mut w, 45)?;
        u16w(&mut w, 45)?;
        u16w(&mut w, 0x808)?;
        u16w(&mut w, 8)?;
        u16w(&mut w, *dos_time)?;
        u16w(&mut w, *dos_date)?;
        u32w(&mut w, *crc)?;
        u32w(&mut w, u32::MAX)?;
        u32w(&mut w, u32::MAX)?;
        u16w(&mut w, name.len() as u16)?;
        u16w(&mut w, 28)?;
        u16w(&mut w, 0)?;
        u16w(&mut w, 0)?;
        u16w(&mut w, 0)?;
        u32w(&mut w, 0)?;
        u32w(&mut w, u32::MAX)?;
        w.write_all(name)?;
        u16w(&mut w, 1)?;
        u16w(&mut w, 24)?;
        u64w(&mut w, *size)?;
        u64w(&mut w, *packed)?;
        u64w(&mut w, *offset)?;
    }
    let end = w.stream_position()?;
    u32w(&mut w, 0x06064b50)?;
    u64w(&mut w, 44)?;
    u16w(&mut w, 45)?;
    u16w(&mut w, 45)?;
    u32w(&mut w, 0)?;
    u32w(&mut w, 0)?;
    u64w(&mut w, central.len() as u64)?;
    u64w(&mut w, central.len() as u64)?;
    u64w(&mut w, end - start)?;
    u64w(&mut w, start)?;
    u32w(&mut w, 0x07064b50)?;
    u32w(&mut w, 0)?;
    u64w(&mut w, end)?;
    u32w(&mut w, 1)?;
    u32w(&mut w, 0x06054b50)?;
    u16w(&mut w, 0)?;
    u16w(&mut w, 0)?;
    u16w(&mut w, u16::MAX)?;
    u16w(&mut w, u16::MAX)?;
    u32w(&mut w, u32::MAX)?;
    u32w(&mut w, u32::MAX)?;
    u16w(&mut w, 0)?;
    w.sync_all()
}
