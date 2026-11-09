use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::process::PidKey;

pub const JOURNAL_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JournalProcess {
    pub pid: u32,
    pub start_time: u64,
}

impl From<PidKey> for JournalProcess {
    fn from(key: PidKey) -> Self {
        Self {
            pid: key.pid,
            start_time: key.start_time,
        }
    }
}

impl From<&PidKey> for JournalProcess {
    fn from(key: &PidKey) -> Self {
        Self {
            pid: key.pid,
            start_time: key.start_time,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JournalAffinity {
    pub pid: u32,
    pub start_time: u64,
    pub mask: u64,
}

impl JournalAffinity {
    pub fn key(&self) -> PidKey {
        PidKey::new(self.pid, self.start_time)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActiveJobRecord {
    pub policy_id: Option<String>,
    pub job_name: String,
    #[serde(default)]
    pub roots: Vec<JournalProcess>,
    #[serde(default)]
    pub original_affinity: Vec<JournalAffinity>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActiveJobJournal {
    pub version: u32,
    pub session_id: String,
    #[serde(default)]
    pub guard_pid: Option<u32>,
    #[serde(default)]
    pub guard_start_time: Option<u64>,
    #[serde(default)]
    pub jobs: Vec<ActiveJobRecord>,
}

impl Default for ActiveJobJournal {
    fn default() -> Self {
        Self {
            version: JOURNAL_VERSION,
            session_id: uuid::Uuid::new_v4().to_string(),
            guard_pid: None,
            guard_start_time: None,
            jobs: Vec::new(),
        }
    }
}

impl ActiveJobJournal {
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(journal) if journal.version == JOURNAL_VERSION => journal,
            Ok(journal) => {
                log::warn!(
                    "unsupported journal version {} at {}",
                    journal.version,
                    path.display()
                );
                Self::default()
            }
            Err(error) => {
                log::warn!("failed to parse journal {}: {error}", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self)?;
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        if let Err(error) = std::fs::rename(&temp, path) {
            let _ = std::fs::remove_file(path);
            std::fs::rename(&temp, path).map_err(|second| {
                anyhow::anyhow!(
                    "rename {} -> {} failed: {error}; retry: {second}",
                    temp.display(),
                    path.display()
                )
            })?;
        }
        Ok(())
    }

    pub fn upsert_job(&mut self, mut record: ActiveJobRecord) {
        if let Some(existing) = self
            .jobs
            .iter_mut()
            .find(|job| job.job_name.eq_ignore_ascii_case(&record.job_name))
        {
            // 自动重应用会重复写入同一条记录；保留首次捕获的原始亲和性，
            // 否则第二次应用会把“原始掩码”覆盖成已经限制后的掩码。
            for root in record.roots.drain(..) {
                if !existing
                    .roots
                    .iter()
                    .any(|current| current.pid == root.pid && current.start_time == root.start_time)
                {
                    existing.roots.push(root);
                }
            }
            for affinity in record.original_affinity.drain(..) {
                if !existing.original_affinity.iter().any(|current| {
                    current.pid == affinity.pid && current.start_time == affinity.start_time
                }) {
                    existing.original_affinity.push(affinity);
                }
            }
            existing.policy_id = record.policy_id;
        } else {
            self.jobs.push(record);
        }
    }

    pub fn remove_job(&mut self, job_name: &str) {
        self.jobs
            .retain(|job| !job.job_name.eq_ignore_ascii_case(job_name));
    }
}

pub fn is_valid_job_name(name: &str) -> bool {
    name.len() <= 200
        && (name.starts_with("Local\\PJH-Policy-")
            || name.starts_with("Local\\PJH-Temp-")
            || name.starts_with("Local\\PJH-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_replaces_same_job() {
        let mut journal = ActiveJobJournal::default();
        journal.upsert_job(ActiveJobRecord {
            policy_id: Some("a".to_owned()),
            job_name: "Local\\PJH-Policy-a".to_owned(),
            roots: vec![JournalProcess {
                pid: 1,
                start_time: 2,
            }],
            original_affinity: Vec::new(),
        });
        journal.upsert_job(ActiveJobRecord {
            policy_id: Some("a".to_owned()),
            job_name: "Local\\PJH-Policy-a".to_owned(),
            roots: Vec::new(),
            original_affinity: Vec::new(),
        });
        assert_eq!(journal.jobs.len(), 1);
        assert_eq!(journal.jobs[0].roots.len(), 1);
    }

    #[test]
    fn upsert_merges_original_affinity() {
        let mut journal = ActiveJobJournal::default();
        let record = |mask: u64| ActiveJobRecord {
            policy_id: Some("a".to_owned()),
            job_name: "Local\\PJH-Policy-a".to_owned(),
            roots: Vec::new(),
            original_affinity: vec![JournalAffinity {
                pid: 10,
                start_time: 20,
                mask,
            }],
        };
        journal.upsert_job(record(0xF_FFFF));
        journal.upsert_job(record(0x3));
        assert_eq!(journal.jobs.len(), 1);
        assert_eq!(journal.jobs[0].original_affinity.len(), 1);
        assert_eq!(journal.jobs[0].original_affinity[0].mask, 0xF_FFFF);
    }

    #[test]
    fn rejects_foreign_job_names() {
        assert!(is_valid_job_name("Local\\PJH-Policy-1"));
        assert!(!is_valid_job_name("Local\\SomeOtherJob"));
    }
}
