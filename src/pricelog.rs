//! Append-only JSONL log of every trade probe — the corpus that bootstraps the
//! later pricing model. Market data only; no Discord user data is written.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;

use crate::trade::model::Probe;

pub struct ProbeLog {
    path: PathBuf,
    lock: Mutex<()>,
}

impl ProbeLog {
    pub fn new(path: impl AsRef<Path>) -> Self {
        ProbeLog { path: path.as_ref().to_path_buf(), lock: Mutex::new(()) }
    }

    /// Appends one probe as a JSON line. Errors are returned, never panicked, so
    /// a logging failure can be downgraded to a warning by the caller.
    pub fn append(&self, probe: &Probe) -> Result<()> {
        let line = serde_json::to_string(probe)?;
        let _guard = self.lock.lock().unwrap();
        let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        writeln!(f, "{line}")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::{MiscFilters, Probe, TradeQuery};

    fn probe(typical: f64) -> Probe {
        Probe {
            query: TradeQuery {
                league: "Standard".into(),
                category: None,
                type_line: Some("Sapphire Ring".into()),
                stats: vec![],
                misc: MiscFilters::default(),
            },
            listing_count: 7,
            typical_divine: typical,
        }
    }

    #[test]
    fn appends_one_json_line_per_probe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("probes.jsonl");
        let log = ProbeLog::new(&path);
        log.append(&probe(10.0)).unwrap();
        log.append(&probe(20.0)).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"typical_divine\":10"));
        assert!(lines[1].contains("Sapphire Ring"));
    }
}
