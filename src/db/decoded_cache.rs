//! Decoded records from immutable MainKey snapshots. One bounded cache is
//! shared by all mounted readers of an engine, not one budget per volume.
use super::{
    fields::{Record, Value},
    writer::snapshot::Snapshot,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Weak},
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct Key(usize, u32, u32, u16);
#[derive(Debug)]
struct Item {
    snapshot: Weak<Snapshot>,
    record: Record,
    bytes: usize,
    referenced: bool,
}
#[derive(Debug, Default)]
pub(super) struct DecodedCache {
    items: HashMap<Key, Item>,
    clock: VecDeque<Key>,
    bytes: usize,
}
impl DecodedCache {
    fn key(snapshot: &Arc<Snapshot>, entry: super::mainkey::Entry) -> Key {
        Key(
            Arc::as_ptr(snapshot) as usize,
            entry.oid,
            entry.block,
            entry.checksum,
        )
    }
    pub(super) fn get(
        &mut self,
        snapshot: &Arc<Snapshot>,
        entry: super::mainkey::Entry,
    ) -> Option<Record> {
        let item = self.items.get_mut(&Self::key(snapshot, entry))?;
        if !Weak::ptr_eq(&item.snapshot, &Arc::downgrade(snapshot)) {
            return None;
        }
        item.referenced = true;
        Some(item.record.clone())
    }
    pub(super) fn insert(
        &mut self,
        snapshot: &Arc<Snapshot>,
        entry: super::mainkey::Entry,
        record: &Record,
    ) {
        let bytes = 160
            + record
                .fields
                .iter()
                .map(|field| {
                    std::mem::size_of::<super::fields::Field>()
                        + match &field.value {
                            Value::String(v) | Value::Text(v) => v.len(),
                            Value::Binary { data, file, .. } => {
                                data.len()
                                    + file
                                        .as_ref()
                                        .map_or(0, |f| f.name.len() + f.source.len() + 4)
                            }
                            Value::TextWithMetadata { data, metadata } => {
                                data.len() + metadata.len()
                            }
                            _ => 0,
                        }
                })
                .sum::<usize>();
        self.insert_bounded(snapshot, entry, record, bytes, 32 * 1024 * 1024, 65536);
    }
    fn insert_bounded(
        &mut self,
        snapshot: &Arc<Snapshot>,
        entry: super::mainkey::Entry,
        record: &Record,
        bytes: usize,
        budget: usize,
        count: usize,
    ) {
        let key = Self::key(snapshot, entry);
        // Large RTF and attachment bodies should not evict the report working set.
        if bytes > 256 * 1024 || bytes > budget || count == 0 || self.items.contains_key(&key) {
            return;
        }
        while self.bytes + bytes > budget || self.items.len() >= count {
            let oldest = self.clock.pop_front().unwrap();
            let item = self.items.get_mut(&oldest).unwrap();
            if item.referenced && item.snapshot.strong_count() != 0 {
                item.referenced = false;
                self.clock.push_back(oldest);
            } else {
                self.bytes -= self.items.remove(&oldest).unwrap().bytes;
            }
        }
        self.bytes += bytes;
        self.clock.push_back(key);
        self.items.insert(
            key,
            Item {
                snapshot: Arc::downgrade(snapshot),
                record: record.clone(),
                bytes,
                referenced: true,
            },
        );
    }
}
