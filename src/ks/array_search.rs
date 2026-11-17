//! Scalar Array.search used by the original mp_addArray helper.
use super::{array_index, Fault, Object, Runtime, Value};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::{Rc, Weak},
};

struct NumericIndex {
    owner: Weak<RefCell<HashMap<String, Value>>>,
    len: usize,
    first: HashMap<i32, usize>,
}
#[derive(Default)]
pub(super) struct NumericSearchCache {
    entries: HashMap<usize, NumericIndex>,
    keys: usize,
}
fn integer_value(v: &Value) -> Option<i32> {
    match v {
        Value::Number(n)
            if n.is_finite()
                && n.fract() == 0.0
                && (i32::MIN as f64..=i32::MAX as f64).contains(n) =>
        {
            Some(*n as i32)
        }
        _ => None,
    }
}
impl NumericSearchCache {
    pub(super) fn indexed_values(&self) -> usize {
        self.keys
    }
    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.keys = 0;
    }
    pub(super) fn invalidate(&mut self, array: &Object) {
        if let Some(old) = self.entries.remove(&(Rc::as_ptr(array) as usize)) {
            self.keys -= old.first.len();
        }
    }
    pub(super) fn append(&mut self, array: &Object, len: usize, values: &[Value]) {
        let id = Rc::as_ptr(array) as usize;
        if self.keys.saturating_add(values.len()) > 1_000_000 {
            self.clear();
            return;
        }
        let Some(entry) = self.entries.get_mut(&id) else {
            return;
        };
        if entry.len != len
            || entry.owner.upgrade().is_none()
            || values.iter().any(|v| integer_value(v).is_none())
        {
            self.invalidate(array);
            return;
        }
        let before = entry.first.len();
        for (offset, value) in values.iter().enumerate() {
            entry
                .first
                .entry(integer_value(value).unwrap())
                .or_insert(len + offset);
        }
        self.keys += entry.first.len() - before;
        entry.len += values.len();
    }
    fn find(&mut self, array: &Object, len: usize, needle: i32) -> Option<Option<usize>> {
        let id = Rc::as_ptr(array) as usize;
        if let Some(entry) = self.entries.get(&id) {
            if entry.len == len
                && entry
                    .owner
                    .upgrade()
                    .is_some_and(|owner| Rc::ptr_eq(&owner, array))
            {
                return Some(entry.first.get(&needle).copied());
            }
        }
        self.invalidate(array);
        if !(128..=1_000_000).contains(&len) {
            return None;
        }
        // Only dense, purely numeric arrays qualify. Sparse arrays and native
        // conversions retain the original search path. Cache capacity only
        // controls retention, never the number of values scripts can hold.
        let map = array.borrow();
        let mut first = HashMap::new();
        let mut count = 0;
        for (key, value) in map.iter() {
            if let Some(i) = array_index(key, len) {
                let n = integer_value(value)?;
                first
                    .entry(n)
                    .and_modify(|old: &mut usize| *old = (*old).min(i))
                    .or_insert(i);
                count += 1;
            }
        }
        if count != len {
            return None;
        }
        if self.entries.len() >= 128 || self.keys + first.len() > 1_000_000 {
            self.clear();
        }
        let result = first.get(&needle).copied();
        self.keys += first.len();
        self.entries.insert(
            id,
            NumericIndex {
                owner: Rc::downgrade(array),
                len,
                first,
            },
        );
        Some(result)
    }
}

impl Runtime<'_> {
    pub(super) fn array_search(&mut self, array: Object, args: Vec<Value>) -> Result<Value, Fault> {
        let needle = args.first().ok_or("Array.search requires a value")?;
        // ScriptArrayObject 0066a962: integer needles compare the integer
        // payload; other needles compare string representations. Extra
        // arguments are ignored by the native one-argument operation.
        let integer = matches!(needle,Value::Number(n) if n.is_finite()&&n.fract()==0.0&&(i32::MIN as f64..=i32::MAX as f64).contains(n));
        match needle {
            Value::Number(_) => {}
            Value::String(_) => {}
            Value::Date(_) | Value::Null | Value::Bool(_) | Value::StoredField(_) | Value::Decimal(_) | Value::Document(..) => {}
            _ => return Err("Array.search supports signed integers, strings and dates; other native conversions are not implemented".into()),
        }
        let map = array.borrow();
        let len = map.get("length").ok_or("array length missing")?.number()? as usize;
        // OID membership is the hot operation of a linked deletion walk.
        // Inspect existing scalar values directly: no temporary string/index
        // allocation for each visited object. Missing slots are null, so they
        // cannot match a nonzero integer. Keep the lowest canonical index.
        if let Value::Number(n) = needle {
            if integer && *n != 0.0 {
                if let Some(found) = self.numeric_arrays.find(&array, len, *n as i32) {
                    return Ok(found.map_or(Value::Null, |i| Value::Number(i as f64)));
                }
                let mut first: Option<usize> = None;
                for (key, value) in map.iter() {
                    self.tick()?;
                    let same = match value {
                        Value::Number(v) => v == n,
                        Value::Bool(v) => u8::from(*v) as f64 == *n,
                        _ => false,
                    };
                    if same {
                        if let Some(i) = array_index(key, len) {
                            first = Some(first.map_or(i, |old| old.min(i)));
                        }
                    }
                }
                return Ok(first.map_or(Value::Null, |i| Value::Number(i as f64)));
            }
        }
        for i in 0..len {
            self.tick()?;
            let item = map.get(&i.to_string()).unwrap_or(&Value::Null);
            let found =
                match (needle, item) {
                    (Value::Number(a), Value::Number(b)) if integer => a == b,
                    (Value::Number(a), Value::Bool(b)) if integer => *a == u8::from(*b) as f64,
                    (Value::Number(a), Value::Null) if integer => *a == 0.0,
                    (Value::Number(_), _) if integer => false,
                    (
                        Value::String(_)
                        | Value::Number(_)
                        | Value::Date(_)
                        | Value::Null
                        | Value::Bool(_)
                        | Value::StoredField(_)
                        | Value::Decimal(_)
                        | Value::Document(..),
                        Value::Number(_)
                        | Value::String(_)
                        | Value::Bool(_)
                        | Value::Null
                        | Value::Date(_)
                        | Value::StoredField(_)
                        | Value::Decimal(_)
                        | Value::Document(..),
                    ) => needle.text() == item.text(),
                    _ => return Err(
                        "Array.search string conversion of host/object values is not implemented"
                            .into(),
                    ),
                };
            if found {
                return Ok(Value::Number(i as f64));
            }
        }
        Ok(Value::Null)
    }
}
