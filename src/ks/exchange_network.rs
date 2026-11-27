use super::*;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Mutex, OnceLock};
static RECEIVING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
pub(super) struct Lease(String);
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut locks) = RECEIVING.get_or_init(Default::default).lock() {
            locks.remove(&self.0);
        }
    }
}
#[derive(Clone)]
pub(super) struct Config {
    pub kind: String,
    pub mail: mail::Settings,
    pub inbox: String,
    pub recipients: Vec<String>,
}
pub(super) struct ReceivedFile {
    pub name: String,
    pub spool: PathBuf,
    pub remote: String,
    pub crc: u32,
    pub size: usize,
}
pub(super) struct Group {
    pub files: Vec<ReceivedFile>,
    pub messages: Vec<usize>,
    pub head: String,
    pub staged: bool,
    pub done: bool,
}
#[derive(Default)]
pub(super) struct State {
    pub config: Option<Config>,
    pub pop: Option<mail::Pop>,
    pub ftp: Option<ftp::Ftp>,
    pub groups: Vec<Group>,
    pub lease: Option<Lease>,
}
fn array(values: Vec<Value>) -> Value {
    let length = values.len();
    let mut map = values
        .into_iter()
        .enumerate()
        .map(|(i, v)| (i.to_string(), v))
        .collect::<HashMap<_, _>>();
    map.insert("length".into(), Value::Number(length as f64));
    Value::Array(Rc::new(RefCell::new(map)))
}
fn object(values: Vec<(&str, Value)>) -> Value {
    Value::Object(Rc::new(RefCell::new(
        values.into_iter().map(|(k, v)| (k.into(), v)).collect(),
    )))
}
pub(super) fn configure(
    kind: &str,
    single: &Value,
    recipients: Vec<String>,
) -> Result<Config, Fault> {
    let mail = mail::Settings {
        smtp: property(single, "smtpserver")?.text(),
        pop: property(single, "popserver")?.text(),
        from: property(single, "mail")?.text(),
        user: property(single, "loginmail")?.text(),
        password: property(single, "passmail")?.text(),
    };
    for value in [
        &mail.smtp,
        &mail.pop,
        &mail.from,
        &mail.user,
        &mail.password,
    ] {
        wire::clean(value)?;
    }
    Ok(Config {
        kind: kind.into(),
        inbox: mail.pop.clone(),
        mail,
        recipients,
    })
}
fn config(access: &Access) -> Result<Config, Fault> {
    access
        .network
        .config
        .clone()
        .ok_or_else(|| "Сетевой транспорт обмена не настроен".into())
}
fn acquire(config: &Config) -> Result<Lease, Fault> {
    let key = format!(
        "{}|{}|{}|{}",
        config.kind,
        if config.kind == "ftp" {
            &config.mail.from
        } else {
            &config.mail.pop
        },
        config.mail.user,
        config.inbox
    );
    let mut locks = RECEIVING
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "Блокировка приёма недоступна")?;
    if !locks.insert(key.clone()) {
        return Err("Приём из этого почтового ящика/каталога уже выполняется".into());
    }
    Ok(Lease(key))
}
fn save(
    work: &Path,
    group: usize,
    index: usize,
    name: String,
    remote: String,
    bytes: &[u8],
) -> Result<ReceivedFile, Fault> {
    if !packet_name(&name) || bytes.len() > wire::MAX_FILE {
        return Err("Недопустимое вложение обмена".into());
    }
    let spool = work.join(format!(
        ".rx-{group}-{index}-{}.blob",
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&spool)
        .map_err(|e| e.to_string())?;
    f.write_all(bytes)
        .and_then(|_| f.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(ReceivedFile {
        name,
        spool,
        remote,
        crc: crc32fast::hash(bytes),
        size: bytes.len(),
    })
}
fn head(files: &[ReceivedFile]) -> Result<String, Fault> {
    let mut names = files
        .iter()
        .map(|f| f.name.to_lowercase())
        .collect::<Vec<_>>();
    names.sort();
    if names.windows(2).any(|w| w[0] == w[1]) {
        return Err("Повторяющиеся имена вложений в комплекте".into());
    }
    if files.len() == 1 && (names[0].ends_with(".7z") || names[0].ends_with(".pkg")) {
        return Ok(files[0].name.clone());
    }
    let first = files
        .iter()
        .find(|f| f.name.to_lowercase().ends_with(".7z.001"))
        .ok_or("Нет первой части многотомного архива .7z.001")?;
    let prefix = &first.name[..first.name.len() - 3];
    for index in 1..=files.len() {
        if !files
            .iter()
            .any(|f| f.name.eq_ignore_ascii_case(&format!("{prefix}{index:03}")))
        {
            return Err("Неполный комплект частей архива".into());
        }
    }
    Ok(first.name.clone())
}
pub(super) fn receive(access: &mut Access) -> Result<Value, Fault> {
    let cfg = config(access)?;
    if access.network.lease.is_some() {
        return Err("Повторная инициализация приёма".into());
    }
    access.network.lease = Some(acquire(&cfg)?);
    let work = access.work.as_ref().ok_or("Exchange workspace missing")?;
    let mut skipped = 0;
    let received;
    if cfg.kind == "email" {
        let mut pop = mail::Pop::connect(&cfg.mail)?;
        received = pop.messages;
        let mut groups: BTreeMap<String, Vec<(usize, usize, usize, Vec<ReceivedFile>)>> =
            BTreeMap::new();
        let mut staged_bytes = 0usize;
        for index in 1..=pop.messages {
            crate::execution::check()?;
            if let Some(message) = mail::parse(&pop.receive(index)?)? {
                let mut files = Vec::new();
                for (name, bytes) in message.files {
                    staged_bytes += bytes.len();
                    if staged_bytes > 2 * 1024 * 1024 * 1024usize {
                        return Err("Почтовый приём превышает 2 ГиБ за один запрос; письма оставлены на сервере".into());
                    }
                    files.push(save(work, index, files.len(), name, String::new(), &bytes)?);
                }
                groups.entry(message.id).or_default().push((
                    index,
                    message.index,
                    message.count,
                    files,
                ));
            }
        }
        for (_, mut messages) in groups {
            messages.sort_by_key(|(_, part, _, _)| *part);
            let count = messages[0].2;
            if messages.len() != count
                || messages
                    .iter()
                    .enumerate()
                    .any(|(i, (_, part, n, _))| *part != i + 1 || *n != count)
            {
                skipped += 1;
                continue;
            }
            let mut files = Vec::new();
            let mut indexes = Vec::new();
            for (index, _, _, part) in messages {
                indexes.push(index);
                files.extend(part);
            }
            let head = head(&files)?;
            access.network.groups.push(Group {
                files,
                messages: indexes,
                head,
                staged: false,
                done: false,
            });
        }
        access.network.pop = Some(pop);
    } else if cfg.kind == "ftp" {
        let mut ftp = ftp::Ftp::connect(&cfg.mail.from, &cfg.mail.user, &cfg.mail.password)?;
        let mut paths = Vec::new();
        fn walk(
            ftp: &mut ftp::Ftp,
            folder: &str,
            depth: usize,
            out: &mut Vec<String>,
        ) -> Result<(), Fault> {
            if depth > 16 {
                return Err("Слишком глубокая структура каталога FTP".into());
            }
            for (name, dir) in ftp.list(folder)? {
                let path = format!("{}/{name}", folder.trim_end_matches('/'));
                if dir {
                    walk(ftp, &path, depth + 1, out)?;
                } else if packet_name(&name) {
                    out.push(path);
                    if out.len() > 10000 {
                        return Err("Слишком много пакетов FTP".into());
                    }
                }
            }
            Ok(())
        }
        walk(&mut ftp, &ftp::path(&cfg.inbox)?, 0, &mut paths)?;
        paths.sort();
        received = paths.len();
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for path in paths {
            let key = if path.to_lowercase().contains(".7z.") {
                path[..path.len() - 3].into()
            } else {
                path.clone()
            };
            groups.entry(key).or_default().push(path);
        }
        for (_, paths) in groups {
            let mut files = Vec::new();
            for path in paths {
                let name = path.rsplit('/').next().unwrap().to_owned();
                let bytes = ftp.get(&path)?;
                files.push(save(
                    work,
                    access.network.groups.len(),
                    files.len(),
                    name,
                    path,
                    &bytes,
                )?);
            }
            match head(&files) {
                Ok(head) => access.network.groups.push(Group {
                    files,
                    messages: Vec::new(),
                    head,
                    staged: false,
                    done: false,
                }),
                Err(_) => {
                    skipped += 1;
                    for f in files {
                        std::fs::remove_file(f.spool).map_err(|e| e.to_string())?;
                    }
                }
            }
        }
        access.network.ftp = Some(ftp);
    } else {
        return Err("Неизвестный сетевой транспорт".into());
    }
    let groups = access
        .network
        .groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            object(vec![
                ("token", Value::Number(i as f64)),
                ("archive", Value::String(g.head.clone())),
            ])
        })
        .collect();
    Ok(object(vec![
        ("groups", array(groups)),
        ("received", Value::Number(received as f64)),
        ("incomplete", Value::Number(skipped as f64)),
    ]))
}
pub(super) fn stage(access: &mut Access, index: usize) -> Result<(), Fault> {
    let work = access.work.as_ref().ok_or("Exchange workspace missing")?;
    let group = access
        .network
        .groups
        .get_mut(index)
        .ok_or("Нет такого комплекта обмена")?;
    if group.staged || group.done {
        return Err("Комплект уже обработан".into());
    }
    let backup = access.backup.as_ref().ok_or("Exchange backup missing")?;
    let day = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let backup = backup.join(format!(
        "exchange-{day}-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&backup).map_err(|e| e.to_string())?;
    for file in &group.files {
        archive::deliver(&file.spool, &backup.join(&file.name))?;
        archive::deliver(&file.spool, &work.join(&file.name))?;
    }
    group.staged = true;
    Ok(())
}
pub(super) fn acknowledge(access: &mut Access, index: usize) -> Result<(), Fault> {
    let group = access
        .network
        .groups
        .get_mut(index)
        .ok_or("Нет такого комплекта обмена")?;
    if !group.staged || group.done {
        return Err("Комплект не готов к подтверждению".into());
    }
    if let Some(pop) = access.network.pop.as_mut() {
        for &index in &group.messages {
            pop.delete(index)?;
        }
    }
    if let Some(ftp) = access.network.ftp.as_mut() {
        // Re-read before deleting: never remove a file replaced during import.
        for file in &group.files {
            let bytes = ftp.get(&file.remote)?;
            if bytes.len() != file.size || crc32fast::hash(&bytes) != file.crc {
                return Err(
                    "FTP: пакет изменился во время импорта; файл оставлен на сервере".into(),
                );
            }
        }
        for file in &group.files {
            ftp.delete(&file.remote)?;
        }
    }
    group.done = true;
    Ok(())
}
pub(super) fn finish(access: &mut Access) -> Result<(), Fault> {
    let result = if let Some(mut pop) = access.network.pop.take() {
        pop.quit()
    } else if let Some(mut ftp) = access.network.ftp.take() {
        ftp.quit()
    } else {
        Ok(())
    };
    access.network.groups.clear();
    access.network.lease = None;
    result
}
impl Runtime<'_> {
    pub(in super::super) fn exchange_network_receive(&mut self) -> Result<Value, Fault> {
        self.exchange_begin_write()?;
        receive(&mut self.exchange_access)
    }
    pub(in super::super) fn exchange_network_stage(
        &mut self,
        values: &[Value],
    ) -> Result<Value, Fault> {
        let index = token(values)?;
        stage(&mut self.exchange_access, index)?;
        Ok(Value::Null)
    }
    pub(in super::super) fn exchange_network_ack(
        &mut self,
        values: &[Value],
    ) -> Result<Value, Fault> {
        let index = token(values)?;
        acknowledge(&mut self.exchange_access, index)?;
        Ok(Value::Null)
    }
    pub(in super::super) fn exchange_mail_send(&self, values: &[Value]) -> Result<Value, Fault> {
        let [to, subject, name] = values else {
            return Err("Exchange mail expects recipient, subject, file".into());
        };
        let cfg = config(&self.exchange_access)?;
        let to = to.text();
        let name = name.text();
        if cfg.kind != "email" || !cfg.recipients.contains(&to) {
            return Err("Получатель не указан в настройках выбранного суда".into());
        }
        let source = self
            .exchange_access
            .work
            .as_ref()
            .ok_or("Exchange workspace missing")?
            .join(&name);
        if !packet_name(&name) {
            return Err("Недопустимое имя пакета".into());
        }
        let bytes = std::fs::read(source).map_err(|e| e.to_string())?;
        mail::send(&cfg.mail, &to, &subject.text(), &name, &bytes)?;
        Ok(Value::Bool(true))
    }
    pub(in super::super) fn exchange_ftp_list(&self, values: &[Value]) -> Result<Value, Fault> {
        let [folder] = values else {
            return Err("Exchange FTP list expects folder".into());
        };
        let cfg = config(&self.exchange_access)?;
        let folder = folder.text();
        if cfg.kind != "ftp" || !cfg.recipients.contains(&folder) {
            return Err("Каталог FTP не указан у выбранного суда".into());
        }
        let mut ftp = ftp::Ftp::connect(&cfg.mail.from, &cfg.mail.user, &cfg.mail.password)?;
        let names = ftp
            .list(&folder)?
            .into_iter()
            .map(|(name, _)| object(vec![("Name", Value::String(name))]))
            .collect();
        ftp.quit()?;
        Ok(array(names))
    }
    pub(in super::super) fn exchange_ftp_send(&self, values: &[Value]) -> Result<Value, Fault> {
        let [folder, name, remote] = values else {
            return Err("Exchange FTP send expects folder, source, target".into());
        };
        let cfg = config(&self.exchange_access)?;
        let folder = folder.text();
        let name = name.text();
        let remote = remote.text();
        if cfg.kind != "ftp"
            || !cfg.recipients.contains(&folder)
            || !packet_name(&name)
            || !packet_name(&remote)
        {
            return Err("Недопустимое назначение FTP".into());
        }
        let bytes = std::fs::read(
            self.exchange_access
                .work
                .as_ref()
                .ok_or("Exchange workspace missing")?
                .join(name),
        )
        .map_err(|e| e.to_string())?;
        let mut ftp = ftp::Ftp::connect(&cfg.mail.from, &cfg.mail.user, &cfg.mail.password)?;
        // FTP has no portable atomic "rename if absent". A per-request nonce
        // prevents concurrent senders from publishing over the same filename.
        let (stem, extension) = remote.rsplit_once('.').ok_or("Нет расширения FTP пакета")?;
        let unique = if let Some((prefix, case)) = stem.rsplit_once('_') {
            format!(
                "{prefix}_{}_{case}.{extension}",
                self.exchange_access.transfer_id
            )
        } else {
            format!("{stem}_{}.{extension}", self.exchange_access.transfer_id)
        };
        ftp.put(&folder, &unique, &bytes)?;
        let _ = ftp.quit();
        Ok(Value::Bool(true))
    }
}
fn token(values: &[Value]) -> Result<usize, Fault> {
    let [v] = values else {
        return Err("Exchange expects token".into());
    };
    let n = v.number()?;
    if !n.is_finite() || n < 0.0 || n.fract() != 0.0 || n > 100000.0 {
        return Err("Invalid exchange token".into());
    }
    Ok(n as usize)
}
