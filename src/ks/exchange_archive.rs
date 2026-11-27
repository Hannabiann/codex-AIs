use super::{Access, Fault, SERIAL};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::Ordering,
    time::Duration,
};

pub(super) fn find_archiver(root: &Path) -> Result<PathBuf, Fault> {
    #[cfg(windows)]
    let candidates = vec![
        root.join("swapsend/7za.exe"),
        root.join("swapget/7za.exe"),
        root.join("7za.exe"),
    ];
    #[cfg(not(windows))]
    let candidates = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .flat_map(|p| [p.join("7zz"), p.join("7z"), p.join("7za")])
        .collect::<Vec<_>>();
    candidates.into_iter().find(|p|p.is_file()).ok_or_else(||"Для обмена требуется 7-Zip: 7za.exe в swapsend/swapget (Windows), 7zz/7z/7za в PATH (Unix)".into())
}
fn run(access: &Access, args: &[String]) -> Result<String, Fault> {
    let work = access.work.as_ref().ok_or("Exchange workspace missing")?;
    let stdout = work.join(".archive-stdout");
    let stderr = work.join(".archive-stderr");
    let mut command = Command::new(
        access
            .archiver
            .as_ref()
            .ok_or("Exchange archiver missing")?,
    );
    command
        .args(args)
        .current_dir(work)
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            File::create(&stdout).map_err(|e| e.to_string())?,
        ))
        .stderr(Stdio::from(
            File::create(&stderr).map_err(|e| e.to_string())?,
        ));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().map_err(|e| format!("7-Zip: {e}"))?;
    let status = loop {
        if let Err(e) = crate::execution::check() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e.into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string().into());
            }
        }
    };
    let read = |path: &Path| -> Result<String, Fault> {
        let mut b = Vec::new();
        File::open(path)
            .map_err(|e| e.to_string())?
            .take(4 * 1024 * 1024)
            .read_to_end(&mut b)
            .map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&b).into_owned())
    };
    let output = read(&stdout)?;
    let error = read(&stderr)?;
    if !status.success() {
        return Err(format!(
            "7-Zip завершился с кодом {}: {} {}",
            status.code().unwrap_or(-1),
            error.trim(),
            output
                .chars()
                .rev()
                .take(1000)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
        )
        .into());
    }
    Ok(output)
}
pub(super) fn xml_name(name: &str) -> bool {
    name.strip_prefix("a_")
        .and_then(|s| s.strip_suffix(".xml"))
        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
}
pub(super) fn pack(access: &Access, oid: u32, extension: &str) -> Result<(), Fault> {
    if !matches!(extension, ".7z" | ".pkg") {
        return Err("Неизвестный формат пакета".into());
    }
    let work = access.work.as_ref().ok_or("Exchange workspace missing")?;
    let entries = std::fs::read_dir(work)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let number = entries
        .iter()
        .filter(|e| e.path().extension().is_some_and(|s| s == &extension[1..]))
        .count();
    let mut xml = entries
        .iter()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            xml_name(&n).then_some(n)
        })
        .collect::<Vec<_>>();
    xml.sort();
    if xml.is_empty() {
        return Err("Не подготовлены XML для передачи".into());
    }
    let target = format!("attach_{number}_{oid}{extension}");
    if work.join(&target).exists() {
        return Err("Пакет обмена уже существует".into());
    }
    if extension == ".pkg" {
        if access.packet_size.is_some() {
            return Err(
                "Для разбиения на почтовые части выберите формат 7z; .pkg передаётся одним файлом"
                    .into(),
            );
        }
        let files = xml
            .into_iter()
            .map(|name| std::fs::read(work.join(&name)).map(|data| (name, data)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let bytes = super::pkg::encode(&files)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(work.join(target))
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let mut args = vec!["a".into(), "-t7z".into(), "-y".into()];
    if let Some(size) = access.packet_size {
        args.push(format!("-v{size}b"));
    }
    args.push(target);
    args.extend(xml);
    run(access, &args)?;
    Ok(())
}
pub(super) fn unpack(access: &Access, name: &str) -> Result<(), Fault> {
    let work = access.work.as_ref().ok_or("Exchange workspace missing")?;
    if !super::packet_name(name) {
        return Err("Недопустимое имя пакета обмена".into());
    }
    // Old 7za (shipped with Kodeks) treats .7z.001 as a Split container and
    // only extracts the inner .7z. Assemble verified consecutive volumes
    // ourselves, then apply the same entry/XML validation as a normal packet.
    if let Some((_, part)) = name.to_ascii_lowercase().rsplit_once(".7z.") {
        if part != "001" {
            return Err("Распаковка должна начинаться с части .7z.001".into());
        }
        let prefix = &name[..name.len() - 3];
        let mut parts = std::fs::read_dir(work)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                (n.is_ascii()
                    && n.len() > prefix.len()
                    && n[..prefix.len()].eq_ignore_ascii_case(prefix))
                .then(|| {
                    n[prefix.len()..]
                        .parse::<usize>()
                        .ok()
                        .map(|i| (i, e.path()))
                })
                .flatten()
            })
            .collect::<Vec<_>>();
        parts.sort_by_key(|(i, _)| *i);
        if parts.is_empty()
            || parts.len() > 10000
            || parts.iter().enumerate().any(|(i, (n, _))| *n != i + 1)
        {
            return Err("Неполный комплект томов 7z".into());
        }
        let joined = format!("assembled_{}.7z", SERIAL.fetch_add(1, Ordering::Relaxed));
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(work.join(&joined))
            .map_err(|e| e.to_string())?;
        let mut total = 0u64;
        for (_, path) in parts {
            let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if !meta.is_file() || meta.file_type().is_symlink() || meta.len() == 0 {
                return Err("Недопустимый том 7z".into());
            }
            total = total
                .checked_add(meta.len())
                .ok_or("Переполнение размера 7z")?;
            if total > 512 * 1024 * 1024 {
                return Err("Многотомный 7z превышает 512 МиБ".into());
            }
            let mut input = File::open(path).map_err(|e| e.to_string())?;
            let mut buffer = [0; 65536];
            loop {
                crate::execution::check()?;
                let n = input.read(&mut buffer).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                output.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
            }
        }
        output.sync_all().map_err(|e| e.to_string())?;
        drop(output);
        return unpack(access, &joined);
    }
    let path = work.join(name);
    let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 512 * 1024 * 1024 {
        return Err("Недопустимый файл пакета обмена".into());
    }
    if name.to_ascii_lowercase().ends_with(".pkg") {
        let files = super::pkg::decode(&std::fs::read(path).map_err(|e| e.to_string())?)?;
        for (_, bytes) in &files {
            validate_xml(bytes)?;
        }
        for (name, bytes) in files {
            let mut out = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(work.join(name))
                .map_err(|e| e.to_string())?;
            out.write_all(&bytes).map_err(|e| e.to_string())?;
        }
        return Ok(());
    }
    let listing = run(access, &["l".into(), "-slt".into(), name.into()])?;
    let entries = listing
        .split_once("----------")
        .ok_or("Не удалось прочитать состав пакета 7z")?
        .1;
    let mut names = std::collections::HashSet::new();
    let mut total = 0u64;
    for line in entries.lines() {
        if let Some(value) = line.strip_prefix("Path = ") {
            if !xml_name(value) || !names.insert(value.to_owned()) || names.len() > 10000 {
                return Err("Пакет должен содержать уникальные a_N.xml без вложенных путей".into());
            }
        }
        if let Some(value) = line.strip_prefix("Size = ") {
            let size = value
                .parse::<u64>()
                .map_err(|_| "Некорректный размер XML в архиве")?;
            if size > 64 * 1024 * 1024 {
                return Err("XML пакета превышает 64 МиБ".into());
            }
            total = total.checked_add(size).ok_or("Размер пакета переполнен")?;
            if total > 512 * 1024 * 1024 {
                return Err("Распакованный пакет превышает 512 МиБ".into());
            }
        }
        if line.starts_with("Symbolic Link = ") || line.starts_with("Hard Link = ") {
            return Err("Ссылки в пакете обмена запрещены".into());
        }
    }
    if names.is_empty() {
        return Err("В пакете нет XML объектов".into());
    }
    run(
        access,
        &["e".into(), "-y".into(), name.into(), "*.xml".into()],
    )?;
    for name in names {
        crate::execution::check()?;
        let p = work.join(name);
        let m = std::fs::symlink_metadata(&p).map_err(|e| e.to_string())?;
        if !m.is_file()
            || m.file_type().is_symlink()
            || p.canonicalize().map_err(|e| e.to_string())?.parent() != Some(work.as_path())
        {
            return Err("Недопустимый распакованный XML".into());
        }
        let data = std::fs::read(p).map_err(|e| e.to_string())?;
        validate_xml(&data)?;
    }
    Ok(())
}
fn validate_xml(data: &[u8]) -> Result<(), Fault> {
    let text = if data
        .windows(5)
        .take(128)
        .any(|w| w.eq_ignore_ascii_case(b"utf-8"))
    {
        std::str::from_utf8(data)
            .map_err(|_| "Некорректный UTF-8 пакета")?
            .to_owned()
    } else {
        crate::db::decode(data)
    };
    super::super::xml_sax::events_bounded(&text, 64 * 1024 * 1024)?;
    Ok(())
}
pub(super) fn deliver(source: &Path, target: &Path) -> Result<(), Fault> {
    if target.exists() {
        return Err("Файл доставки уже существует".into());
    }
    let temp = target.with_extension(format!(
        "{}.{}.part",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<(), Fault> {
        let mut input = File::open(source).map_err(|e| e.to_string())?;
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        let mut buffer = [0; 65536];
        loop {
            crate::execution::check()?;
            let n = input.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            out.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        }
        out.sync_all().map_err(|e| e.to_string())?;
        drop(out);
        super::super::bras_export::publish_complete(&temp, target).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        File::open(target.parent().ok_or("Delivery directory missing")?)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(())
    })();
    let _ = std::fs::remove_file(temp);
    result
}
