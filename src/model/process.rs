use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PidKey {
    pub pid: u32,
    pub start_time: u64,
}

impl PidKey {
    pub fn new(pid: u32, start_time: u64) -> Self {
        Self { pid, start_time }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProcessRecord {
    pub key: PidKey,
    pub parent_pid: u32,
    pub name: String,
    pub cpu_percent: f32,
    pub memory: u64,
    pub commit: u64,
    pub path: Option<PathBuf>,
}

impl ProcessRecord {
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            "<unknown>"
        } else {
            &self.name
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProcessSnapshot {
    pub records: Vec<ProcessRecord>,
    pub by_pid: HashMap<u32, usize>,
}

impl ProcessSnapshot {
    pub fn new(records: Vec<ProcessRecord>) -> Self {
        let mut by_pid = HashMap::with_capacity(records.len());
        for (index, record) in records.iter().enumerate() {
            by_pid.insert(record.key.pid, index);
        }
        Self { records, by_pid }
    }

    pub fn get(&self, pid: u32) -> Option<&ProcessRecord> {
        self.by_pid
            .get(&pid)
            .and_then(|index| self.records.get(*index))
    }

    pub fn find_by_key(&self, key: PidKey) -> Option<&ProcessRecord> {
        self.get(key.pid)
            .filter(|record| record.key.start_time == key.start_time)
    }
}
