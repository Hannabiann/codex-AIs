//! Portable subset of MSXML used by the stored XML/BRAS libraries.
//! These are data objects, not general COM activation or external resolvers.
use super::*;
use std::collections::BTreeMap;

type Handle = Rc<RefCell<XmlObject>>;
const NOT_WELL_FORMED: u32 = 0xc00ce223;
const NO_SCHEMA: u32 = 0xc00ce224;
const INVALID_CONTENT: u32 = 0xc00ce014;

#[derive(Clone, Debug, Default)]
struct ParseError {
    code: u32,
    reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    const SCHEMA: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="Case"><xs:complexType><xs:sequence><xs:element name="Number" type="xs:nonNegativeInteger"/><xs:element name="Name" type="xs:string" minOccurs="0" maxOccurs="2"/></xs:sequence><xs:attribute name="version" fixed="7" use="required"/></xs:complexType></xs:element></xs:schema>"#;

    fn document(source: &str, cache: Option<Handle>) -> XmlDocument {
        let mut d = XmlDocument {
            schemas: cache,
            validate_on_parse: false,
            ..Default::default()
        };
        assert!(load(&mut d, source.into()));
        d
    }
    fn cache() -> Handle {
        Rc::new(RefCell::new(XmlObject::Schemas(BTreeMap::from([(
            String::new(),
            compile(SCHEMA, "").unwrap(),
        )]))))
    }
    #[test]
    fn xsd_checks_values_structure_attributes_and_occurrences() {
        let c = cache();
        for xml in [
            "<Case version='7'><Number>12</Number></Case>",
            "<Case version='7'><Number>0</Number><Name>Суд</Name><Name/></Case>",
        ] {
            assert_eq!(validate(&document(xml, Some(c.clone()))).code, 0, "{xml}");
        }
        for xml in [
            "<Case version='7'/>",
            "<Case version='7'><Number>-1</Number></Case>",
            "<Case version='7'><Number>x</Number></Case>",
            "<Case><Number>1</Number></Case>",
            "<Case version='8'><Number>1</Number></Case>",
            "<Case version='7'><Name/><Number>1</Number></Case>",
            "<Case version='7'><Number>1</Number><Name/><Name/><Name/></Case>",
            "<Wrong/>",
            "<Case version='7' extra='1'><Number>1</Number></Case>",
            "<Case version='7'>text<Number>1</Number></Case>",
            "<Case version='7'><Number xmlns='urn:wrong'>1</Number></Case>",
        ] {
            assert_ne!(validate(&document(xml, Some(c.clone()))).code, 0, "{xml}");
        }
    }
    #[test]
    fn script_uses_cache_and_validate_without_changing_parse_error() {
        let mut r = Runtime::new("/test");
        r.scopes[0]
            .borrow_mut()
            .insert("schemaText".into(), Value::String(SCHEMA.into()));
        let source = r#"
            var schema = new OLEobject('Msxml2.DOMDocument.6.0');
            if (!schema.loadXML(schemaText)) throw 'schema parse';
            var cache = new OLEobject('Msxml2.XMLSchemaCache.6.0');
            cache.add(null, schema);
            var xml = new OLEobject('Msxml2.DOMDocument.6.0');
            xml.async = false; xml.validateOnParse = false; xml.schemas = cache;
            if (!xml.loadXML('<Case version="7"><Number>-1</Number></Case>')) throw 'parse';
            if (xml.validate().errorCode == 0) throw 'validation bypass';
            if (xml.parseError.errorCode != 0) throw 'parseError changed';
            xml.validateOnParse = true;
            if (xml.loadXML('<Case/>')) throw 'load validation bypass';
            if (xml.parseError.errorCode == 0) throw 'missing parse error';
            if (!xml.loadXML('<Case version="7"><Number>1</Number></Case>')) throw 'valid';
            if (xml.validate().errorCode != 0) throw 'valid schema';
            cache.remove('');
            if (xml.validate().errorCode == 0) throw 'missing schema';
        "#;
        let ast = r.parse_cached_named(source, "xml-fixture.ks").unwrap();
        assert_eq!(r.render(&ast).unwrap(), "");
    }
    #[test]
    fn malformed_schema_and_external_resources_fail_explicitly() {
        assert!(compile("<not-schema/>", "").is_err());
        assert!(compile(SCHEMA, "urn:wrong").is_err());
        for xsd in [
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:include schemaLocation="file:///never-read.xsd"/></xs:schema>"#,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:complexType name="T"><xs:assert test="false()"/></xs:complexType></xs:schema>"#,
        ] {
            assert!(compile(xsd, "").is_err(), "{xsd}");
        }
        for xml in [
            "<broken>",
            "<!--x--><?test?><!DOCTYPE a SYSTEM 'file:///never-read'><a/>",
            "<!DOCTYPE a [<!ENTITY e 'x'>]><a>&e;</a>",
        ] {
            assert!(parse(xml).is_err(), "{xml}");
        }
        assert!(parse("<a><![CDATA[<!DOCTYPE text>]]></a>").is_ok());
        assert!(construct(&[Value::String("WScript.Shell".into())]).is_err());
        assert_ne!(validate(&document("<Case/>", None)).code, 0);
    }
    #[test]
    #[ignore = "requires a local XSD and XML corpus with expected MSXML validation results"]
    fn external_msxml_corpus() {
        let root =
            std::path::PathBuf::from(std::env::var("CODEX_XSD_CORPUS").expect("CODEX_XSD_CORPUS"));
        let source = std::fs::read_to_string(root.join("schema.xsd")).unwrap();
        let cache = Rc::new(RefCell::new(XmlObject::Schemas(BTreeMap::from([(
            String::new(),
            compile(&source, "").unwrap(),
        )]))));
        let expected = std::fs::read_to_string(root.join("expected.tsv")).unwrap();
        let mut count = 0;
        for line in expected.lines() {
            let (name, valid) = line.split_once('\t').unwrap();
            let source = std::fs::read_to_string(root.join(name)).unwrap();
            let d = document(&source, Some(cache.clone()));
            assert_eq!(
                validate(&d).code == 0,
                valid == "true",
                "{name}: {}",
                validate(&d).reason
            );
            count += 1;
        }
        assert!(count > 0);
    }
}
impl ParseError {
    fn new(code: u32, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
        }
    }
    fn value(&self) -> Value {
        Value::ReadOnlyObject(Rc::new(RefCell::new(HashMap::from([
            ("errorCode".into(), Value::Number(self.code as i32 as f64)),
            ("reason".into(), Value::String(self.reason.clone())),
        ]))))
    }
}

pub(super) enum XmlObject {
    Document(XmlDocument),
    Schemas(BTreeMap<String, SchemaEntry>),
}
impl std::fmt::Debug for XmlObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Document(d) => write!(f, "XMLDocument({} bytes)", d.source.len()),
            Self::Schemas(s) => write!(f, "XMLSchemaCache({} schemas)", s.len()),
        }
    }
}
pub(super) struct SchemaEntry {
    source: String,
    schema: xmlschema::Schema,
}
pub(super) struct XmlDocument {
    source: String,
    parsed: Option<oxml::Document>,
    schemas: Option<Handle>,
    validate_on_parse: bool,
    asynchronous: bool,
    parse_error: ParseError,
}
impl Default for XmlDocument {
    fn default() -> Self {
        Self {
            source: String::new(),
            parsed: None,
            schemas: None,
            validate_on_parse: true,
            asynchronous: true,
            parse_error: ParseError::default(),
        }
    }
}

fn parse(source: &str) -> Result<oxml::Document, String> {
    // MSXML 6's ProhibitDTD remains enabled. Skip only actual prolog comments
    // and processing instructions, not occurrences inside text/CDATA.
    let mut prolog = source.trim_start_matches('\u{feff}').trim_start();
    loop {
        let (start, end) = if prolog.starts_with("<!--") {
            (4, "-->")
        } else if prolog.starts_with("<?") {
            (2, "?>")
        } else {
            break;
        };
        let Some(at) = prolog[start..].find(end) else {
            break;
        };
        prolog = prolog[start + at + end.len()..].trim_start();
    }
    if prolog.starts_with("<!DOCTYPE") {
        return Err("XML DTD is prohibited; external entities are not resolved".into());
    }
    let mut limits = oxml::Limits::default();
    limits.max_input_bytes = Some(128 * 1024 * 1024);
    limits.max_nodes = Some(1_000_000);
    oxml::parse_with(source, limits).map_err(|e| e.to_string())
}

fn compile(source: &str, namespace: &str) -> Result<SchemaEntry, Fault> {
    let doc = parse(source)?;
    let root = doc.root_element().ok_or("XML schema has no root")?;
    let name = doc
        .element_name(root)
        .ok_or("XML schema root is not an element")?;
    if name.local != "schema"
        || name.namespace.as_deref() != Some("http://www.w3.org/2001/XMLSchema")
    {
        return Err("XMLSchemaCache.add requires an XSD schema document".into());
    }
    let unsupported = xmlschema::support::unsupported(&doc);
    if !unsupported.is_empty() {
        return Err(format!(
            "XSD constraints are not supported: {}",
            unsupported
                .iter()
                .map(|u| u.construct.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into());
    }
    // The cache accepts in-memory schemas. It must never turn schemaLocation
    // into unrestricted file/network access or silently omit an import.
    if doc.descendants().any(|id| {
        doc.element_name(id).is_some_and(|n| {
            n.namespace.as_deref() == Some("http://www.w3.org/2001/XMLSchema")
                && matches!(
                    n.local.as_str(),
                    "import" | "include" | "redefine" | "override"
                )
        })
    }) {
        return Err(
            "External XSD imports/includes are not supported by the in-memory schema cache".into(),
        );
    }
    if doc.descendants().any(|id| {
        doc.element_name(id).is_some_and(|n| {
            n.namespace.as_deref() == Some("http://www.w3.org/2001/XMLSchema")
                && (matches!(n.local.as_str(), "any" | "anyAttribute")
                    || doc.attribute(id, "form").is_some()
                    || doc
                        .attribute(id, "mixed")
                        .is_some_and(|v| v != "false" && v != "0"))
        })
    }) || doc.attribute(root, "attributeFormDefault") == Some("qualified")
        || (!namespace.is_empty() && doc.attribute(root, "elementFormDefault") != Some("qualified"))
    {
        return Err(
            "XSD wildcards, mixed content and local namespace overrides are not supported".into(),
        );
    }
    let mut schema = xmlschema::parse_schema(source).map_err(|e| e.to_string())?;
    if schema.target_namespace.as_deref().unwrap_or("") != namespace {
        return Err("XMLSchemaCache namespace differs from XSD targetNamespace".into());
    }
    // xmlschema 0.0.14 checks undeclared attributes only when a wildcard is
    // present. An empty namespace set expresses the XSD default: reject them.
    fn attributes(content: &mut xmlschema::Content) {
        if let xmlschema::Content::Sequence(p)
        | xmlschema::Content::All(p)
        | xmlschema::Content::Choice(p) = content
        {
            for p in p {
                particle(p);
            }
        }
    }
    fn particle(p: &mut xmlschema::Particle) {
        if p.any_attribute.is_none() && !matches!(*p.content, xmlschema::Content::Any) {
            p.any_attribute = Some(xmlschema::Wildcard {
                namespaces: xmlschema::NamespaceConstraint::List(Vec::new()),
                process: xmlschema::ProcessContents::Strict,
            });
        }
        attributes(&mut p.content);
    }
    for p in schema.elements.values_mut() {
        particle(p);
    }
    for c in schema.named_complex_types.values_mut() {
        attributes(c);
    }
    Ok(SchemaEntry {
        source: source.into(),
        schema,
    })
}

fn validate(doc: &XmlDocument) -> ParseError {
    let Some(parsed) = &doc.parsed else {
        return ParseError::new(NOT_WELL_FORMED, "XML document has no valid root element");
    };
    let Some(cache) = &doc.schemas else {
        return ParseError::new(NO_SCHEMA, "No XML schema is associated with this document");
    };
    let cache = cache.borrow();
    let XmlObject::Schemas(entries) = &*cache else {
        unreachable!()
    };
    let namespace = parsed
        .root_element()
        .and_then(|id| parsed.element_name(id))
        .and_then(|n| n.namespace.as_deref())
        .unwrap_or("");
    let Some(entry) = entries.get(namespace) else {
        return ParseError::new(
            NO_SCHEMA,
            format!("No XML schema for namespace {namespace:?}"),
        );
    };
    // The dependency matches declared child elements by local name and does
    // not reject character data in element-only content. Enforce both rules
    // before accepting its result; the supported schemas have uniform names.
    fn check(
        doc: &oxml::Document,
        id: oxml::NodeId,
        p: &xmlschema::Particle,
        namespace: Option<&str>,
    ) -> Result<(), String> {
        let name = doc.element_name(id).ok_or("XML element missing")?;
        if name.namespace.as_deref() != namespace {
            return Err(format!("{}: unexpected element namespace", name.local));
        }
        for a in doc.attributes(id) {
            let n = doc.name(a.name).ok_or("XML attribute name missing")?;
            if let Some(ns) = n.namespace.as_deref() {
                if ns != "http://www.w3.org/2001/XMLSchema-instance" {
                    return Err(format!("{}: unexpected attribute namespace", n.local));
                }
                match n.local.as_str() {
                    "schemaLocation" | "noNamespaceSchemaLocation" => {}
                    "nil" if a.value == "false" || a.value == "0" => {}
                    // Do not silently accept dynamic types or unimplemented
                    // nil semantics. The BRAS schema does not use either.
                    _ => return Err(format!("xsi:{} is not supported with this value", n.local)),
                }
            }
        }
        if let xmlschema::Content::Sequence(children)
        | xmlschema::Content::All(children)
        | xmlschema::Content::Choice(children) = &*p.content
        {
            for &child in doc.children(id) {
                if matches!(doc.kind(child), Some(oxml::NodeKind::Text(text)) if !text.trim().is_empty())
                {
                    return Err(format!("{}: character data is not allowed", name.local));
                }
                if let Some(n) = doc.element_name(child) {
                    if let Some(decl) = children.iter().find(|p| p.name == n.local) {
                        check(doc, child, decl, namespace)?;
                    }
                }
            }
        }
        Ok(())
    }
    if let Some(root) = parsed.root_element() {
        if let Some(p) = parsed
            .element_name(root)
            .and_then(|n| entry.schema.elements.get(&n.local))
        {
            if let Err(e) = check(parsed, root, p, entry.schema.target_namespace.as_deref()) {
                return ParseError::new(INVALID_CONTENT, e);
            }
        }
    }
    let report = xmlschema::validate(parsed, &entry.schema);
    if report.is_valid() {
        ParseError::default()
    } else {
        ParseError::new(
            INVALID_CONTENT,
            report
                .violations
                .iter()
                .take(8)
                .map(|v| format!("{}: {}", v.path, v.message))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
}

fn load(doc: &mut XmlDocument, source: String) -> bool {
    doc.parsed = None;
    doc.source.clear();
    doc.parse_error = ParseError::default();
    match parse(&source) {
        Ok(parsed) => {
            doc.source = source;
            doc.parsed = Some(parsed);
        }
        Err(e) => {
            doc.parse_error = ParseError::new(NOT_WELL_FORMED, e);
            return false;
        }
    }
    if doc.validate_on_parse && doc.schemas.is_some() {
        doc.parse_error = validate(doc);
    }
    doc.parse_error.code == 0
}

pub(super) fn from_source(source: String, validate_on_parse: bool) -> Result<Value, Fault> {
    let mut doc = XmlDocument {
        validate_on_parse,
        asynchronous: false,
        ..Default::default()
    };
    if !source.is_empty() && !load(&mut doc, source) {
        return Err(doc.parse_error.reason.into());
    }
    Ok(Value::Xml(Rc::new(RefCell::new(XmlObject::Document(doc)))))
}

pub(super) fn construct(values: &[Value]) -> Result<Value, Fault> {
    let [Value::String(progid)] = values else {
        return Err("OLEobject expects a ProgID string".into());
    };
    let id = progid.to_ascii_lowercase();
    let object = match id.as_str() {
        "msxml2.domdocument.6.0" | "msxml2.freethreadeddomdocument.6.0" => {
            XmlObject::Document(XmlDocument::default())
        }
        "msxml2.xmlschemacache.6.0" => XmlObject::Schemas(BTreeMap::new()),
        _ => {
            return Err(format!(
                "OLEobject {progid:?} is not implemented; arbitrary COM activation is unavailable"
            )
            .into())
        }
    };
    Ok(Value::Xml(Rc::new(RefCell::new(object))))
}

pub(super) fn member(object: &Handle, key: &str) -> Result<Value, Fault> {
    match &*object.borrow() {
        XmlObject::Document(doc) => match key {
            "xml" => Ok(Value::String(doc.source.clone())),
            "parseError" => Ok(doc.parse_error.value()),
            "schemas" => Ok(doc
                .schemas
                .as_ref()
                .map(|s| Value::Xml(s.clone()))
                .unwrap_or(Value::Null)),
            "validateOnParse" => Ok(Value::Bool(doc.validate_on_parse)),
            "async" => Ok(Value::Bool(doc.asynchronous)),
            "resolveExternals" => Ok(Value::Bool(false)),
            "readyState" => Ok(Value::Number(if doc.parsed.is_some() { 4.0 } else { 0.0 })),
            "loadXML" | "validate" | "setProperty" | "getProperty" => {
                Ok(Value::XmlMethod(object.clone(), key.into()))
            }
            _ => Err(format!("MSXML document member {key} is not implemented").into()),
        },
        XmlObject::Schemas(entries) => match key {
            "length" => Ok(Value::Number(entries.len() as f64)),
            "add" | "get" | "remove" | "namespaceURI" => {
                Ok(Value::XmlMethod(object.clone(), key.into()))
            }
            _ => Err(format!("MSXML schema cache member {key} is not implemented").into()),
        },
    }
}

pub(super) fn set_property(object: &Handle, key: &str, value: Value) -> Result<(), Fault> {
    let schemas = if key == "schemas" {
        match &value {
            Value::Null => None,
            Value::Xml(other)
                if !Rc::ptr_eq(object, other)
                    && matches!(&*other.borrow(), XmlObject::Schemas(_)) =>
            {
                Some(other.clone())
            }
            _ => return Err("XML document schemas requires an XMLSchemaCache".into()),
        }
    } else {
        None
    };
    let mut object = object.borrow_mut();
    let XmlObject::Document(doc) = &mut *object else {
        return Err("XML schema cache properties are read-only".into());
    };
    match key {
        "schemas" => doc.schemas = schemas,
        "validateOnParse" => doc.validate_on_parse = value.truth(),
        "async" => doc.asynchronous = value.truth(),
        "resolveExternals" if !value.truth() => {}
        _ => {
            return Err(
                format!("MSXML document property {key} is not supported with this value").into(),
            )
        }
    }
    Ok(())
}

impl Runtime<'_> {
    pub(super) fn xml_ole_call(
        &mut self,
        object: Handle,
        method: &str,
        values: &[Value],
    ) -> Result<Value, Fault> {
        crate::execution::check()?;
        if matches!(&*object.borrow(), XmlObject::Schemas(_)) {
            match (method, values) {
                ("add", [namespace, source]) => {
                    let source = match source {
                        Value::Xml(doc) if !Rc::ptr_eq(&object, doc) => match &*doc.borrow() {
                            XmlObject::Document(doc) => doc.source.clone(),
                            _ => return Err("XMLSchemaCache.add requires an XML document".into()),
                        },
                        _ => {
                            return Err(
                                "XMLSchemaCache.add requires an in-memory XML document".into()
                            )
                        }
                    };
                    let namespace = namespace.text();
                    let entry = compile(&source, &namespace)?;
                    crate::execution::check()?;
                    let mut cache = object.borrow_mut();
                    let XmlObject::Schemas(entries) = &mut *cache else {
                        unreachable!()
                    };
                    entries.insert(namespace, entry);
                    return Ok(Value::Null);
                }
                ("get", [namespace]) => {
                    let cache = object.borrow();
                    let XmlObject::Schemas(entries) = &*cache else {
                        unreachable!()
                    };
                    return match entries.get(&namespace.text()) {
                        Some(entry) => from_source(entry.source.clone(), false),
                        None => Ok(Value::Null),
                    };
                }
                ("remove", [namespace]) => {
                    let mut cache = object.borrow_mut();
                    let XmlObject::Schemas(entries) = &mut *cache else {
                        unreachable!()
                    };
                    entries.remove(&namespace.text());
                    return Ok(Value::Null);
                }
                ("namespaceURI", [index]) => {
                    let index = index.number()?;
                    if !index.is_finite() || index < 0.0 || index.fract() != 0.0 {
                        return Err("Invalid schema cache index".into());
                    }
                    let cache = object.borrow();
                    let XmlObject::Schemas(entries) = &*cache else {
                        unreachable!()
                    };
                    return entries
                        .keys()
                        .nth(index as usize)
                        .cloned()
                        .map(Value::String)
                        .ok_or_else(|| "Schema cache index is out of range".into());
                }
                _ => return Err(format!("XMLSchemaCache.{method}: unsupported arguments").into()),
            }
        }
        match (method, values) {
            ("loadXML", [Value::String(source)]) => {
                let mut object = object.borrow_mut();
                let XmlObject::Document(doc) = &mut *object else {
                    unreachable!()
                };
                let loaded = load(doc, source.clone());
                crate::execution::check()?;
                Ok(Value::Bool(loaded))
            }
            ("validate", []) => {
                let object = object.borrow();
                let XmlObject::Document(doc) = &*object else {
                    unreachable!()
                };
                // validate() returns a separate error object, without replacing parseError.
                let result = validate(doc).value();
                crate::execution::check()?;
                Ok(result)
            }
            ("setProperty", [Value::String(name), value]) => {
                match name.to_ascii_lowercase().as_str() {
                    "validateonparse" => set_property(&object, "validateOnParse", value.clone())?,
                    "resolveexternals" => set_property(&object, "resolveExternals", value.clone())?,
                    "prohibitdtd" if value.truth() => {}
                    _ => return Err(format!("MSXML setProperty {name} is not supported").into()),
                }
                Ok(Value::Null)
            }
            ("getProperty", [Value::String(name)]) => match name.to_ascii_lowercase().as_str() {
                "validateonparse" => member(&object, "validateOnParse"),
                "resolveexternals" => member(&object, "resolveExternals"),
                "prohibitdtd" => Ok(Value::Bool(true)),
                _ => Err(format!("MSXML getProperty {name} is not supported").into()),
            },
            _ => Err(format!("MSXML document {method}: unsupported arguments").into()),
        }
    }
}
