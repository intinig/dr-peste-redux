//! Append-only JSONL corpus of per-listing market observations — the data the
//! learning layer mines. Market data only; never any Discord/member secret.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::trade::model::ListingMod;

/// Where an observation came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Paste,
    Harvest,
}

/// One real market listing, captured for the learning corpus.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub timestamp_unix: u64,
    pub league: String,
    pub base_type: Option<String>,
    pub category: Option<String>,
    pub mods: Vec<ListingMod>,
    pub price_divine: f64,
    pub source: Source,
}

/// Append-only JSONL log of observations. Mutex-guarded; failures are returned,
/// never panicked, so the caller can downgrade to a warning.
pub struct ObservationLog {
    path: PathBuf,
    lock: Mutex<()>,
}

impl ObservationLog {
    pub fn new(path: impl AsRef<Path>) -> Self {
        ObservationLog {
            path: path.as_ref().to_path_buf(),
            lock: Mutex::new(()),
        }
    }

    /// Appends one observation as a JSON line. Errors are returned, never
    /// panicked, so a logging failure can be downgraded to a warning by the caller.
    pub fn append(&self, obs: &Observation) -> Result<()> {
        let line = serde_json::to_string(obs)?;
        let _guard = self.lock.lock().unwrap();
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{line}")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::ListingMod;

    fn obs(price: f64) -> Observation {
        Observation {
            timestamp_unix: 0,
            league: "Standard".into(),
            base_type: Some("Chiming Staff".into()),
            category: Some("Staves".into()),
            mods: vec![ListingMod {
                stat_id: "explicit.stat_1".into(),
                tier: Some(2),
                roll: Some(123.0),
            }],
            price_divine: price,
            source: Source::Paste,
        }
    }

    #[test]
    fn appends_one_json_line_per_observation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obs.jsonl");
        let log = ObservationLog::new(&path);
        log.append(&obs(10.0)).unwrap();
        log.append(&obs(20.0)).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        // Round-trips back to the same struct.
        let back: Observation = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(back, obs(10.0));
        assert!(lines[0].contains("\"source\":\"paste\""));
        assert!(lines[1].contains("Staves"));
    }
}
