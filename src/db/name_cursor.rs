//! Incremental in-order walk of a native name index. Keep repeated keys: a
//! native SQL ordered cursor can visit one object under several indexed names.
use super::{u16at, u32at, MainKey};
use std::collections::HashSet;

struct Frame {
    bytes: Vec<u8>,
    count: usize,
    branch: bool,
    base: usize,
    position: usize,
    previous: usize,
    child_visited: bool,
    depth: usize,
}
pub(crate) struct NameCursor {
    tree: MainKey,
    area: u16,
    stack: Vec<Frame>,
    visited: HashSet<u32>,
    allowed: Vec<u32>,
    named: Vec<bool>,
}
impl NameCursor {
    pub fn new(tree: MainKey, area: u16, allowed: Vec<u32>) -> Result<Self, String> {
        if allowed.windows(2).any(|w| w[0] >= w[1]) {
            return Err("name cursor identities must be sorted and unique".into());
        }
        let root = tree
            .directory
            .chunks_exact(10)
            .find(|c| u16at(c, 6) == area && u16at(c, 8) == 1)
            .map(|c| u32at(c, 2))
            .ok_or("name cursor index missing")?;
        let mut cursor = Self {
            tree,
            area,
            stack: Vec::new(),
            visited: HashSet::new(),
            named: vec![false; allowed.len()],
            allowed,
        };
        if root != 0 {
            cursor.push(root, 0)?;
        }
        Ok(cursor)
    }
    fn push(&mut self, block: u32, depth: usize) -> Result<(), String> {
        if block == 0 || depth >= 32 || self.visited.len() >= 32768 || !self.visited.insert(block) {
            return Err("name cursor depth/node budget/cycle".into());
        }
        let bytes = self.tree.block(block, b'L', 1024 * 1024)?;
        if self.tree.bytes > 128 * 1024 * 1024 {
            return Err("name cursor byte budget exceeded".into());
        }
        if bytes.len() < 7 || u16at(&bytes, 0) != 1 || u16at(&bytes, 2) != self.area {
            return Err("name cursor index header mismatch".into());
        }
        let count = u16at(&bytes, 4) as usize;
        let branch = bytes[6] != 0;
        let base = 7 + count * 2;
        if base > bytes.len() {
            return Err("truncated name cursor offsets".into());
        }
        self.stack.push(Frame {
            bytes,
            count,
            branch,
            base,
            position: 0,
            previous: 0,
            child_visited: false,
            depth,
        });
        Ok(())
    }
    pub fn next(&mut self) -> Result<Option<(Vec<u8>, u32)>, String> {
        loop {
            crate::execution::check()?;
            let Some(frame) = self.stack.last_mut() else {
                return Ok(None);
            };
            let child_depth = frame.depth + 1;
            if frame.position == frame.count {
                let child = if frame.branch {
                    if frame.base + frame.previous + 4 > frame.bytes.len() {
                        return Err("truncated final name cursor child".into());
                    }
                    Some(u32at(&frame.bytes, frame.base + frame.previous))
                } else {
                    None
                };
                self.stack.pop();
                if let Some(child) = child {
                    self.push(child, child_depth)?;
                }
                continue;
            }
            let end = u16at(&frame.bytes, 7 + frame.position * 2) as usize;
            let start = frame.previous + if frame.branch { 4 } else { 0 };
            if end < start + 4 || frame.base + end > frame.bytes.len() {
                return Err("invalid name cursor entry bounds".into());
            }
            if frame.branch && !frame.child_visited {
                let child = u32at(&frame.bytes, frame.base + frame.previous);
                frame.child_visited = true;
                self.push(child, child_depth)?;
                continue;
            }
            let oid = u32at(&frame.bytes, frame.base + start);
            frame.position += 1;
            frame.previous = end;
            frame.child_visited = false;
            if let Ok(index) = self.allowed.binary_search(&oid) {
                self.named[index] = true;
                return Ok(Some((
                    frame.bytes[frame.base + start + 4..frame.base + end].to_vec(),
                    oid,
                )));
            }
        }
    }
    pub fn missing(&self) -> impl Iterator<Item = u32> + '_ {
        self.allowed
            .iter()
            .zip(&self.named)
            .filter_map(|(&oid, &named)| (!named).then_some(oid))
    }
}
