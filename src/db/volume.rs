//! Read-only DB40 metadata. Native Volume.name: 005edc8a / 005f2ce0.
//! updatenum: 005edeb7, loaded from header +0x21 at 005f2d50.
use std::{fs::File, io::Read, path::Path};
pub fn metadata(path: &Path, id: u32) -> Result<crate::ks::database::Volume, String> {
    super::writer::ensure_recovered(path)?;
    let _publication = super::writer::open_guard()?;
    let mut header = [0u8; 0x121];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if &header[..4] != b"DB40" {
        return Err("unsupported volume metadata format".into());
    }
    let title = &header[0xa1..];
    let end = title
        .iter()
        .position(|b| *b == 0)
        .ok_or("unterminated volume title")?;
    let name = path.to_string_lossy().into_owned();
    let name = if cfg!(windows) {
        if let Some(rest) = name.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{rest}")
        } else {
            name.strip_prefix(r"\\?\").unwrap_or(&name).to_string()
        }
    } else {
        name
    };
    Ok(crate::ks::database::Volume {
        id,
        name,
        title: super::decode(&title[..end]),
        update: u16::from_le_bytes(header[0x21..0x23].try_into().unwrap()),
    })
}
