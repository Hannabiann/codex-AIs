mod auth;
mod classifier_html;
mod db;
mod document;
mod execution;
mod http_form;
mod ks;
mod monitor;
mod listen_config;
mod product;
mod rich_text;
mod runtime_logs;
mod server;
mod settings;
mod vm;
mod worker_config;
use std::{path::PathBuf, sync::Arc};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(String::as_str).unwrap_or("serve");
    if matches!(command, "--version" | "-V") {
        println!("{} {}", product::NAME, product::version());
        return Ok(());
    }
    if command == "recover" {
        let root = args.get(2).ok_or("usage: codex-AIs recover DATA_ROOT")?;
        let count = db::writer::recover_directory(std::path::Path::new(root))?;
        println!("Recovered {count} DB40 journals");
        return Ok(());
    }
    if command == "audit-local-paths" {
        let root = args.get(2).ok_or("usage: audit-local-paths DATA_ROOT")?;
        let engine = db::Db4Engine::init_from_config_root(root)?;
        fn visit(reg: &db::registry::Registry, id: u32, parent: &str) -> Result<(), String> {
            let key = reg.key(id)?;
            let path = format!("{parent}/{}", key.name);
            for v in &key.values {
                if v.value.contains(":\\") || v.value.starts_with("\\\\") {
                    println!("{path}/{}\t{:?}", v.name, v.value);
                }
            }
            for child in &key.children {
                visit(reg, *child, &path)?;
            }
            Ok(())
        }
        visit(&engine.registry, engine.registry.root, "")?;
        for d in &engine.virtual_dirs {
            if d.path.trim_end_matches('/') == "/99" {
                println!(
                    "/99: mapped_to={}, bases={:?}, properties={:?}",
                    d.mapped_to, d.bases, d.properties
                );
            }
        }
        return Ok(());
    }
    if command == "inspect-sql" {
        use ks::database::DatabaseHost;
        if !(6..=7).contains(&args.len()) {
            return Err(
                "usage: inspect-sql DATA_ROOT VIRTUAL_DIR SQL_FILE OUTPUT [MOUNTS_FILE]".into(),
            );
        }
        let engine = Arc::new(
            db::Db4Engine::init_from_config_root(&args[2]).map_err(std::io::Error::other)?,
        );
        let dir = engine
            .resolve_virtual_dir(&args[3])
            .ok_or("virtual directory absent")?
            .clone();
        let mut context =
            db::context::Contexts::new(engine, &dir).map_err(std::io::Error::other)?;
        if let Some(path) = args.get(6) {
            for volume in std::fs::read_to_string(path)?
                .lines()
                .filter(|s| !s.trim().is_empty())
            {
                context
                    .open(0, volume.trim(), &[false, false, true, false])
                    .map_err(|e| std::io::Error::other(format!("{e:?}")))?;
            }
        }
        let query = std::fs::read_to_string(&args[4])?;
        let result = context.select(0, &query).map_err(std::io::Error::other)?;
        let mut out = format!("area={:?}, rows={}\n", result.area, result.oids.len());
        for (i, oid) in result.oids.iter().enumerate().take(1000) {
            out.push_str(&format!(
                "{oid}\t{:?}\t{:?}\n",
                result.cells.get(i),
                context.document_volume(0, *oid).map(|v| v.name)
            ));
        }
        std::fs::write(&args[5], out)?;
        return Ok(());
    }
    if command == "inspect-links" {
        if args.len() != 4 {
            return Err("usage: inspect-links VOLUME_PATH OID".into());
        }
        let r = db::read_file_oid(std::path::Path::new(&args[2]), args[3].parse()?)?
            .ok_or("OID absent")?;
        println!(
            "area={} name={}",
            r.area,
            r.string(1).map(db::decode).unwrap_or_default()
        );
        for f in r.fields.iter() {
            if let db::fields::Value::Integer(n) = f.value {
                println!("{}\t{n}", f.id);
            }
        }
        return Ok(());
    }
    if command == "inspect-host-config" {
        let engine = db::Db4Engine::init_from_config_root(std::env::var("KODEKS_DATA_DIR")?)?;
        for id in &engine.registry.key(engine.registry.root)?.children {
            let key = engine.registry.key(*id)?;
            println!("key: {}", key.name);
            if key.name.to_lowercase().contains("mime") {
                for v in &key.values {
                    println!("{}={}", v.name, v.value);
                }
                for child in &key.children {
                    println!("child: {:?}", engine.registry.key(*child)?);
                }
            }
        }
        for dir in engine
            .virtual_dirs
            .iter()
            .filter(|d| d.path.trim_end_matches('/') == "/20a")
        {
            for (k, v) in &dir.properties {
                if k.to_lowercase().contains("find") || k.to_lowercase().contains("where") {
                    println!("{k}={v:?}");
                }
            }
        }
        return Ok(());
    }
    if command == "extract-forms" {
        if !(4..=5).contains(&args.len()) {
            return Err("usage: extract-forms VOLUME_PATH OUTPUT_DIRECTORY [AREA]".into());
        }
        let path = std::path::Path::new(&args[2]);
        let out = std::path::Path::new(&args[3]);
        std::fs::create_dir_all(out)?;
        let mut catalog = String::new();
        let area = args
            .get(4)
            .map(|s| s.parse::<u16>())
            .transpose()?
            .unwrap_or(1002);
        for oid in db::mainkey::MainKey::open(path)?.area_candidates(area, 10000)? {
            let Some(record) = db::read_file_oid(path, oid)? else {
                continue;
            };
            if record.area != area {
                continue;
            }
            catalog.push_str(&format!(
                "{}\t{}\t{:?}\n",
                oid,
                record.string(1).map(db::decode).unwrap_or_default(),
                record
                    .fields
                    .iter()
                    .filter(|f| f.id == if area == 8 { 20 } else { 3 })
                    .map(|f| format!("{:?}", f.value))
                    .collect::<Vec<_>>()
            ));
            for field in record.fields.iter() {
                if field.wire_type == 4 {
                    use std::io::Write;
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(out.join(format!("{}-{}.ks", oid, field.id)))?
                        .write_all(db::decode(document::bytes(field)?).as_bytes())?;
                }
            }
        }
        std::fs::write(out.join("catalog.tsv"), catalog)?;
        return Ok(());
    }
    if command == "extract-file" {
        if args.len() != 5 {
            return Err("usage: extract-file VOLUME_PATH OBJECT OUTPUT".into());
        }
        let path = std::path::Path::new(&args[2]).canonicalize()?;
        let mut engine = db::Db4Engine::default();
        let (_, key) = engine
            .mount_readonly(
                path.parent().ok_or("volume parent missing")?,
                path.file_name()
                    .ok_or("volume name missing")?
                    .to_str()
                    .ok_or("invalid volume name")?,
            )
            .map_err(std::io::Error::other)?;
        let record = engine
            .named(&key, &args[3])
            .map_err(std::io::Error::other)?
            .ok_or("named source missing")?;
        eprintln!(
            "Exporting source: area={}, OID={:?}, code={}",
            record.area,
            record.integer(0x400),
            record.code().is_some()
        );
        use std::io::Write;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&args[4])?
            .write_all(record.source().ok_or("no source")?)?;
        return Ok(());
    }
    if command == "inspect-paths" {
        let root = std::env::var("KODEKS_DATA_DIR")?;
        let engine = db::Db4Engine::init_from_config_root(root).map_err(std::io::Error::other)?;
        for id in &engine.registry.key(engine.registry.root)?.children {
            if let Some(path) = engine.registry.query(*id, "Path")? {
                println!("{}\t{}\t{}", id, engine.registry.key(*id)?.name, path.value);
            }
        }
        return Ok(());
    }
    if command == "inspect-registry" {
        if args.len() != 4 {
            return Err("usage: inspect-registry KEY VALUE_NAME".into());
        }
        let root = std::env::var("KODEKS_DATA_DIR")?;
        let engine = db::Db4Engine::init_from_config_root(root).map_err(std::io::Error::other)?;
        let id = engine
            .registry
            .open(&args[2])
            .map_err(std::io::Error::other)?
            .ok_or("registry key absent")?;
        for entry in &engine
            .registry
            .key(id)
            .map_err(std::io::Error::other)?
            .values
        {
            if entry.name == args[3] {
                println!("{}: {:?}", entry.kind, entry.value);
            }
        }
        return Ok(());
    }
    if matches!(command, "--help" | "-h" | "help") {
        println!("codex-AIs [serve|inspect|extract VOLUME OBJECT source|code OUTPUT]\nDefault: serve. KODEKS_DATA_DIR selects the data directory.\nDiagnostic: diagnose [URL] (default /20a/); KODEKS_TRACE=1 traces library loads\nInspection: audit-ks VOLUME_PATH | inspect-schema VOLUME_PATH [AREA]\nSource commands: check-ks FILE [utf8|cp1251] | run-ks FILE [utf8|cp1251]\nIndexed commands: lookup-oid VOLUME_PATH OID | extract-oid VOLUME OID OUTPUT\nDocument commands: preview-oid VOLUME_PATH OID OUTPUT.html | inspect-oid VOLUME_PATH OID | export-attr VOLUME_PATH OID ATTR_ID WIRE_TYPE raw|metadata|rtf OUTPUT");
        return Ok(());
    }
    if command == "preview-oid" {
        if args.len() != 5 {
            return Err("usage: preview-oid VOLUME_PATH OID OUTPUT.html".into());
        }
        let record = db::read_file_oid(std::path::Path::new(&args[2]), args[3].parse()?)
            .map_err(std::io::Error::other)?
            .ok_or("OID absent or deleted")?;
        let html = document::plain_html(&record).map_err(std::io::Error::other)?;
        use std::io::Write;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&args[4])?;
        output.write_all(html.as_bytes())?;
        output.sync_all()?;
        return Ok(());
    }
    if command == "inspect-schema" {
        if !(args.len() == 3 || args.len() == 4) {
            return Err("usage: inspect-schema VOLUME_PATH [AREA_NAME_OR_ID]".into());
        }
        let schema = db::schema::Schema::read(std::path::Path::new(&args[2]))
            .map_err(std::io::Error::other)?;
        let areas = if let Some(key) = args.get(3) {
            vec![schema.area(key).ok_or("area absent from volume schema")?]
        } else {
            schema.areas.values().collect()
        };
        println!("AREA_ID\tAREA_NAME\tFIELD_ID\tFIELD_NAME\tNATIVE_TYPE\tFLAGS\tREFERENCE");
        for a in areas {
            for f in a.fields.values() {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    a.id, a.name, f.id, f.name, f.kind, f.flags, f.reference
                );
            }
        }
        return Ok(());
    }
    if command == "audit-ks" {
        if args.len() != 3 {
            return Err("usage: audit-ks VOLUME_PATH".into());
        }
        let path = std::path::Path::new(&args[2]);
        let entries = db::mainkey::MainKey::open(path)
            .map_err(std::io::Error::other)?
            .entries(50000)
            .map_err(std::io::Error::other)?;
        let (mut parsed, mut failed, mut binary_only, mut no_script) = (0, 0, 0, 0);
        for entry in entries.into_iter().filter(|e| e.block != u32::MAX) {
            let Some(r) = db::read_file_oid(path, entry.oid).map_err(std::io::Error::other)? else {
                continue;
            };
            if !matches!(r.area, 901 | 1010) {
                continue;
            }
            let Some(source) = r.source() else {
                if r.code().is_some() {
                    binary_only += 1;
                } else {
                    no_script += 1;
                }
                continue;
            };
            let source = db::decode(source);
            match ks::parser::parse(&source) {
                Ok(_) => parsed += 1,
                Err(e) => {
                    failed += 1;
                    println!(
                        "OID {} {}: {}",
                        entry.oid,
                        r.string(1).map(db::decode).unwrap_or_default(),
                        e
                    );
                }
            }
        }
        println!("KS source audit: {parsed} parsed, {failed} syntax failures, {binary_only} compiled-only, {no_script} without script field");
        if failed > 0 {
            return Err("KS source audit has failures".into());
        }
        return Ok(());
    }
    if command == "inspect-oid" || command == "export-attr" {
        let expected = if command == "inspect-oid" { 4 } else { 8 };
        if args.len() != expected {
            return Err("usage: inspect-oid VOLUME_PATH OID | export-attr VOLUME_PATH OID ATTR_ID WIRE_TYPE raw|metadata|rtf OUTPUT".into());
        }
        let record = db::read_file_oid(std::path::Path::new(&args[2]), args[3].parse()?)
            .map_err(std::io::Error::other)?
            .ok_or("OID absent or deleted")?;
        if command == "inspect-oid" {
            print!("{}", document::describe(&record));
        } else {
            let data = document::export(&record, args[4].parse()?, args[5].parse()?, &args[6])
                .map_err(std::io::Error::other)?;
            // Never overwrite a source volume or any existing output file.
            use std::io::Write;
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&args[7])?;
            output.write_all(data)?;
            output.sync_all()?;
            eprintln!("Exported {} bytes", data.len());
        }
        return Ok(());
    }
    if command == "lookup-oid" {
        if args.len() != 4 {
            return Err("usage: lookup-oid VOLUME_PATH OID".into());
        }
        let mut index = db::mainkey::MainKey::open(std::path::Path::new(&args[2]))
            .map_err(std::io::Error::other)?;
        let oid = args[3].parse::<u32>()?;
        match index
            .lookup_including_deleted(oid)
            .map_err(std::io::Error::other)?
        {
            Some(entry) if entry.block == u32::MAX => {
                println!("OID {oid}: deletion marker, checksum {:#x}", entry.checksum)
            }
            Some(entry) => {
                let off = index.record_offset(entry).map_err(std::io::Error::other)?;
                println!(
                    "OID {}: block {}, offset {:#x}, header checksum {:#x}",
                    oid, entry.block, off, entry.checksum
                );
            }
            None => println!("OID {oid}: absent or deleted"),
        }
        println!("Index reads: {}, bytes: {}", index.reads, index.bytes);
        return Ok(());
    }
    if command == "check-ks" || command == "run-ks" {
        if !(args.len() == 3 || args.len() == 4 && matches!(args[3].as_str(), "utf8" | "cp1251")) {
            return Err("usage: check-ks|run-ks FILE [utf8|cp1251]".into());
        }
        let file = args
            .get(2)
            .ok_or("usage: check-ks|run-ks FILE [utf8|cp1251]")?;
        let bytes = std::fs::read(file)?;
        let source = if args.get(3).map(String::as_str) == Some("cp1251") {
            db::decode(&bytes)
        } else {
            String::from_utf8(bytes)?
        };
        let ast = ks::parser::parse(&source).map_err(|e| {
            std::io::Error::other(format!(
                "{}:{}:{}: {}",
                file,
                source[..e.offset.min(source.len())]
                    .bytes()
                    .filter(|&b| b == b'\n')
                    .count()
                    + 1,
                e.offset,
                e.message
            ))
        })?;
        if command == "check-ks" {
            println!("KS parsed: {} top-level statements", ast.len());
        } else {
            print!(
                "{}",
                ks::runtime::Runtime::new("/test")
                    .run(&ast)
                    .map_err(std::io::Error::other)?
            );
        }
        return Ok(());
    }
    match command {
        "serve" | "inspect" if args.len() <= 2 => {}
        "diagnose" if args.len() <= 3 => {}
        "extract-oid" if args.len() == 5 => {}
        "extract" if args.len() == 6 && matches!(args[4].as_str(), "source" | "code") => {}
        _ => {
            return Err(
                "usage: codex-AIs [serve|inspect|extract VOLUME OBJECT source|code OUTPUT]".into(),
            )
        }
    }
    let host = std::env::var("KODEKS_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let root = std::env::var_os("KODEKS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    eprintln!(
        "codex-AIs {}; data directory: {}",
        product::version(),
        root.display()
    );
    let engine = Db4Engine::init_from_config_root(&root).map_err(std::io::Error::other)?;
    if matches!(command, "serve" | "diagnose") {
        runtime_logs::initialize(&root).map_err(std::io::Error::other)?;
    }
    match Some(command) {
        Some("diagnose") => {
            let target = args.get(2).map(String::as_str).unwrap_or("/20a/");
            let response = server::route(&engine, target);
            println!("{} {}", response.status, target);
            println!(
                "{}",
                if response.mime.contains("windows-1251") {
                    db::decode(&response.body)
                } else {
                    String::from_utf8_lossy(&response.body).into_owned()
                }
            );
            if response.status != "200 OK" {
                return Err("diagnostic request did not complete successfully".into());
            }
        }
        Some("extract-oid") => {
            let record = engine
                .read_oid(args[3].parse()?, &args[2])
                .map_err(std::io::Error::other)?
                .ok_or("OID not found")?;
            std::fs::write(&args[4], record.source().ok_or("no source field")?)?;
        }
        Some("extract") => {
            if args.len() != 6 {
                return Err("usage: codex-AIs extract VOLUME OBJECT source|code OUTPUT".into());
            }
            let record = engine
                .named(&args[2], &args[3])
                .map_err(std::io::Error::other)?
                .ok_or("object not found")?;
            let bytes = match args[4].as_str() {
                "source" => record.source(),
                "code" => record.code(),
                _ => return Err("kind must be source or code".into()),
            }
            .ok_or("requested field not found")?;
            std::fs::write(&args[5], bytes)?;
            eprintln!("Wrote {} bytes", bytes.len());
        }
        Some("inspect") => {
            for line in &engine.mount_report {
                println!("{line}");
            }
            for d in &engine.virtual_dirs {
                println!("{}\t{}\t{}\t{}", d.path, d.kind, d.mapped_to, d.title);
            }
        }
        Some("serve") => {
            let selected = listen_config::configured(Some(&engine.registry))
                .map_err(std::io::Error::other)?;
            eprintln!("HTTP port: {} ({})", selected.port, selected.source);
            server::run_server(Arc::new(engine), &host, selected.port).await?;
        }
        _ => {
            return Err(
                "commands: inspect | extract VOLUME OBJECT source|code OUTPUT | serve".into(),
            )
        }
    }
    Ok(())
}
use db::Db4Engine;
