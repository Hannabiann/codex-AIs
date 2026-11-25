//! Embedded form lists retain their contents until the HTML writer selects a
//! page. Queries, annotations, exports and tab visibility use the full list.
use super::{
    record_list::{ListIds, RecordList},
    Fault, Object, Runtime, Value,
};
use std::{cell::RefCell, rc::Rc};

fn objects(n: usize) -> String {
    let noun = if (11..=14).contains(&(n % 100)) {
        "объектов"
    } else {
        match n % 10 {
            1 => "объект",
            2..=4 => "объекта",
            _ => "объектов",
        }
    };
    format!("{n} {noun}")
}

impl Runtime<'_> {
    pub(super) fn form_list_value(&self, value: Value) -> Result<Value, Fault> {
        if let Value::Host(rows @ crate::ks::host::Handle::Rows(_)) = value {
            let (scope, area, ids) = rows.list_snapshot()?;
            // SQL already imposed the requested order. An empty order vector
            // here would make the HTML writer sort the result again by name.
            Ok(Value::List(Rc::new(RefCell::new(RecordList {
                scope,
                area,
                order: Some(vec![u16::MAX]),
                conditions: vec![],
                query_all: false,
                ids: ListIds::new(Some(ids)),
            }))))
        } else {
            Ok(value)
        }
    }
    pub(super) fn list_attribute_values(&self, scope: u32, args: &[Value]) -> Result<Value, Fault> {
        let [Value::List(list), attr] = args else {
            return Err("FindAllValuesByAttribute expects a List and attribute number".into());
        };
        let list = list.borrow();
        if list.scope != scope {
            return Err("List belongs to another database".into());
        }
        let n = attr.number()?;
        if !n.is_finite() || n.fract() != 0.0 || !(1.0..=65535.0).contains(&n) {
            return Err("Invalid classifier attribute".into());
        }
        let host = self
            .dbhost
            .as_ref()
            .ok_or("Classifier requires database")?
            .borrow();
        let schema = host.shared_schema(scope)?;
        let definition = schema
            .areas
            .get(&list.area)
            .and_then(|a| a.fields.get(&(n as u16)))
            .ok_or("Classifier attribute absent")?;
        if definition.kind != 5 {
            return Err("Classifier attribute is not a link".into());
        }
        let mut seen = std::collections::HashSet::new();
        let mut ids = Vec::new();
        for oid in self.list_ids(&list)? {
            crate::execution::check()?;
            if let Some(record) = host.read(scope, oid)? {
                for field in record
                    .fields
                    .iter()
                    .filter(|f| f.id == n as u16 && f.wire_type == 5)
                {
                    if let crate::db::fields::Value::Integer(id) = field.value {
                        if id != 0 && seen.insert(id) {
                            ids.push(id);
                        }
                    }
                }
            }
        }
        Ok(Value::List(Rc::new(RefCell::new(RecordList {
            scope,
            area: definition.reference,
            order: None,
            conditions: vec![],
            query_all: false,
            ids: ListIds::new(Some(ids)),
        }))))
    }
    pub(super) fn form_embedded_list(
        &mut self,
        field: &Value,
        index: usize,
    ) -> Result<Value, Fault> {
        let Value::RichField(field) = field else {
            return Err("Expected generated list field".into());
        };
        let value = field
            .borrow()
            .parts
            .iter()
            .filter_map(|part| part.borrow().get("List").cloned())
            .nth(index)
            .ok_or("Form list no longer exists")?;
        let Value::List(list) = self.form_list_value(value)? else {
            return Err("Expected form list".into());
        };
        // Reconstruct the current list on each request; URLs are not handles
        // into an executor's cache and cannot expose a previous user's list.
        let list = list.borrow();
        let copy = RecordList {
            scope: list.scope,
            area: list.area,
            order: list.order.clone(),
            conditions: vec![],
            query_all: false,
            ids: ListIds::new(Some(self.list_ids(&list)?)),
        };
        Ok(Value::List(Rc::new(RefCell::new(copy))))
    }
    pub(super) fn capture_form_list(
        &mut self,
        value: Value,
        lists: &mut Option<Vec<Value>>,
    ) -> Result<String, Fault> {
        let Value::List(list) = &value else {
            return Err("Expected form list".into());
        };
        if self.list_len(&list.borrow())? == 0 {
            return Ok(String::new());
        }
        if let Some(lists) = lists {
            let marker = format!("<!--codex-form-list:{}-->", lists.len());
            lists.push(value);
            Ok(marker)
        } else {
            self.list_html(&list.borrow())
        }
    }

    fn form_list_setting(&mut self, name: &str, list: &Value) -> Result<Value, Fault> {
        let function = self.get(name)?;
        self.call_host(function, vec![list.clone()], None, name.into())
    }

    fn form_page_url(&mut self, offset: i64) -> Result<String, Fault> {
        // AddStart in the vendor library prints into Output rather than
        // returning a string. Build the same URL without writing outside the
        // HTML writer (which may itself target a stream).
        let request = self.get("Request")?;
        let query = self.member(request, "QueryArgs".into())?.text();
        let input = self.get("INPUT")?.text();
        let query = if input.is_empty() {
            query
        } else {
            format!("{}&{input}", query.split('&').next().unwrap_or(""))
        };
        let mut args = query
            .split('&')
            .filter(|arg| {
                !arg.split('=')
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case("start"))
            })
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if offset != 0 {
            args.push(format!("start={offset}"));
        }
        Ok(crate::monitor::escape(&format!(
            "{}?{}",
            self.path.trim_matches('/'),
            args.join("&")
        )))
    }

    pub(super) fn form_list_html(
        &mut self,
        list: &RecordList,
        writer: Option<&Object>,
    ) -> Result<String, Fault> {
        self.form_list_html_at(list, writer, 0)
    }
    pub(super) fn form_list_html_at(
        &mut self,
        list: &RecordList,
        writer: Option<&Object>,
        list_index: usize,
    ) -> Result<String, Fault> {
        // A plain conversion must never inherit the HTTP page offset. In
        // particular isEmpty() runs while the form's tabs are being built.
        let Some(writer) = writer.filter(|w| w.borrow().contains_key("\0setListConverter")) else {
            return self.list_html(list);
        };
        let ids = self.list_ids(list)?;
        // A SQL list reports distinct objects but its sorted cursor may have
        // several rows for one object's different attribute values.
        let count = if list.order.as_deref() == Some(&[u16::MAX]) {
            ids.iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len()
        } else {
            ids.len()
        };
        if count == 0 {
            return Ok(String::new());
        }
        // The writer may sort, but must not mutate a list shared with a form
        // function, another tab or an export in the same request.
        let copy = Rc::new(RefCell::new(RecordList {
            scope: list.scope,
            area: list.area,
            order: list.order.clone(),
            conditions: vec![],
            query_all: false,
            ids: ListIds::new(Some(ids)),
        }));
        let value = Value::List(copy.clone());
        if list
            .order
            .as_ref()
            .is_none_or(|order| order.is_empty() || order == &[0])
        {
            let sort = self.get("sort")?;
            let order = if matches!(sort, Value::Null) {
                self.form_list_setting("GetListOrder", &value)?
            } else {
                sort
            };
            let order = order.number()?;
            if order.is_finite() && order >= 0.0 && order <= u16::MAX as f64 {
                self.list_sort(&mut copy.borrow_mut(), order as u16)?;
                copy.borrow_mut().order = Some(vec![order as u16]);
            }
        }
        let size = self.form_list_setting("GetListSize", &value)?;
        let size = size.number()?;
        let size = if size.is_finite() && size > 0.0 {
            (size as usize).max(1)
        } else {
            count
        };
        let start = self.get("start")?.text().parse::<i64>().unwrap_or(0);
        let mut ids = self.list_ids(&copy.borrow())?;
        if start < 0 && count > size && list.area != 0 {
            // Native traverses a reverse-sorted list backwards. This also
            // reverses equal-key groups; reversing the whole page is wrong.
            ids.reverse();
            let order = copy.borrow().order.clone().unwrap_or_default();
            for attr in order.iter().rev() {
                ids = self.ordered_ids(ids, list.scope, *attr)?;
            }
        }
        let first = if size >= count {
            0
        } else if start < 0 {
            count.saturating_sub(start.unsigned_abs() as usize)
        } else {
            (start as usize).min(ids.len())
        };
        let end = first.saturating_add(size).min(ids.len());
        let page = RecordList {
            scope: list.scope,
            area: list.area,
            order: list.order.clone(),
            conditions: vec![],
            query_all: false,
            ids: ListIds::new(Some(ids[first..end].to_vec())),
        };
        let mut html = self.list_html(&page)?;
        let list_id = format!("cai1-{list_index}");
        let parent = writer
            .borrow()
            .get("document")
            .cloned()
            .unwrap_or(Value::Null);
        let parent_id = if matches!(parent, Value::Null) {
            0
        } else {
            self.member(parent, "NumDoc".into())?.number()? as u32
        };
        let tab = writer
            .borrow()
            .get("tab")
            .cloned()
            .unwrap_or(Value::Number(0.0));
        {
            // The KS global object also contains configuration and functions.
            // Only HTTP form/query parameters belong in a navigation URL.
            let request = self.get("Request")?;
            let query = self.member(request, "QueryArgs".into())?.text();
            let input = self.get("INPUT")?.text();
            let args = format!("{query}&{input}");
            let Value::Object(raw) = super::packing_vars::construct(&[Value::String(args)])? else {
                unreachable!()
            };
            if !self.request_parameters.is_empty() {
                raw.borrow_mut().clear();
                for key in &self.request_parameters {
                    if let Some(value) = self.scopes[0].borrow().get(key) {
                        if matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_)) {
                            raw.borrow_mut().insert(key.clone(), value.clone());
                        }
                    }
                }
            }
            let params = Rc::new(RefCell::new(
                raw.borrow()
                    .iter()
                    .filter(|(k, _)| {
                        !matches!(
                            k.to_ascii_lowercase().as_str(),
                            "spack"
                                | "_r"
                                | "target"
                                | "find"
                                | "doc"
                                | "hdoc"
                                | "navigator"
                                | "start"
                        )
                    })
                    .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
                    .collect::<std::collections::HashMap<_, _>>(),
            ));
            if parent_id != 0 {
                let mut p = params.borrow_mut();
                p.insert("nd".into(), Value::Number(parent_id as f64));
                p.insert("nh".into(), tab.clone());
            }
            let context = ["a0", "intelsearch", "c"]
                .iter()
                .find_map(|key| {
                    params
                        .borrow()
                        .get(*key)
                        .map(Value::text)
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or_default();
            let context = if context.is_empty() {
                String::new()
            } else {
                format!("&amp;c={}", super::packing_vars::escape(&context)?)
            };
            for (offset, oid) in ids[first..end].iter().enumerate() {
                let position = first + offset;
                let reverse = start < 0 && count > size;
                let nav_index = if reverse {
                    count - 1 - position
                } else {
                    position
                };
                {
                    let mut p = params.borrow_mut();
                    p.insert("listid".into(), Value::String(list_id.clone()));
                    p.insert(
                        "listpos".into(),
                        Value::String(format!("{}{nav_index}", if reverse { "-" } else { "" })),
                    );
                    p.insert("lsz".into(), Value::Number(count as f64));
                }
                let packed_params = super::packing_vars::serialize(&params.borrow())?;
                let escape = self.get("escape")?;
                let packed = self
                    .call_host(
                        escape,
                        vec![Value::String(packed_params)],
                        None,
                        "escape".into(),
                    )?
                    .text();
                let spack = format!(
                    "{}{}{kind}{packed}",
                    u8::from(nav_index > 0),
                    u8::from(nav_index + 1 < ids.len()),
                    kind = u8::from(parent_id == 0)
                );
                let from = format!("?d&amp;nd={oid}\"");
                let to = format!(
                    "?d&amp;nd={oid}&amp;prevDoc={oid}&amp;spack={}{context}\"",
                    crate::monitor::escape(&spack)
                );
                html = html.replacen(&from, &to, 1);
            }
        }
        let body = html
            .split_once("<UL>")
            .map(|(_, s)| s.strip_suffix("</UL>").unwrap_or(s))
            .unwrap_or("");
        let mut out = format!("<B>{}</B><UL>", objects(count));
        if count > size
            && parent_id != 0
            && list.area != 0
            && list.area != u16::MAX
            && !writer
                .borrow()
                .get("filter_not_need")
                .is_some_and(|v| v.number().ok() == Some(1.0))
        {
            let params = format!("nd={parent_id}&nh={}&start={start}&", tab.text());
            let action = crate::monitor::escape(&format!("{}?find", self.path.trim_matches('/')));
            let hidden = [
                ("find", String::new()),
                ("filter", "1".into()),
                ("nd", parent_id.to_string()),
                ("nh", tab.text()),
                ("id", list_id),
                ("start", start.to_string()),
                ("razdel", String::new()),
                ("param", params),
            ];
            let inputs = hidden
                .iter()
                .map(|(name, value)| {
                    format!(
                        "<INPUT TYPE=\"HIDDEN\" NAME=\"{name}\" VALUE=\"{}\">",
                        crate::monitor::escape(value)
                    )
                })
                .collect::<String>();
            out=format!("<TABLE BORDER=0 STYLE=\"margin-left:0; margin-right:0\"><TR><TD><B>{}</B></TD><TD><FORM METHOD=\"GET\" TARGET=\"_top\" ACTION=\"{action}\">{inputs}<INPUT TYPE=\"SUBMIT\" NAME=\"filtr\" VALUE=\"Фильтр/Сортировка\"></FORM></TD></TR></TABLE><UL>",objects(count));
        }
        if size < count && first > 0 {
            let previous = if start < 0 {
                -(start
                    .unsigned_abs()
                    .saturating_add(size as u64)
                    .min(count as u64) as i64)
            } else {
                start - size as i64
            };
            let n = if start < 0 { first.min(size) } else { size };
            let url = self.form_page_url(previous)?;
            out.push_str(&format!(
                "<B><A HREF=\"{url}\">Предыдущие {}</A></B><BR><BR>",
                objects(n)
            ));
        }
        out.push_str(body);
        if size < count && end < count {
            let next = if start < 0 {
                end as i64 - count as i64
            } else {
                end as i64
            };
            let url = self.form_page_url(next)?;
            out.push_str(&format!(
                "<P><B><A HREF=\"{url}\">Следующие {}</A></B>",
                objects((count - end).min(size))
            ));
        }
        if count > size {
            let pages = count.div_ceil(size);
            let current = if start < 0 {
                first / size
            } else {
                start as usize / size
            };
            let low = current.saturating_sub(10).min(pages.saturating_sub(20));
            let high = low.saturating_add(20).min(pages);
            out.push_str("<CENTER>Переход к странице: ");
            if low > 0 {
                out.push_str(&format!(
                    "<A HREF=\"{}\">1</A> ... ",
                    self.form_page_url(0)?
                ));
            }
            for page in low..high {
                if page == current {
                    out.push_str(&format!(" <B><FONT SIZE=+1>{}</FONT></B> ", page + 1));
                } else {
                    out.push_str(&format!(
                        " <A HREF=\"{}\">{}</A> ",
                        self.form_page_url((page * size) as i64)?,
                        page + 1
                    ));
                }
            }
            if high < pages {
                out.push_str(&format!(
                    " ... <A HREF=\"{}\">{pages}</A>",
                    self.form_page_url(((pages - 1) * size) as i64)?
                ));
            }
            out.push_str("</CENTER><BR>");
        }
        out.push_str("</UL>");
        Ok(out)
    }
}
