//! Record interrupted plugin startup attempts without attributing a core failure to a plugin.

use crate::plugins::package::{read_json, write_json};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Default, Deserialize, Serialize)]
struct Record {
    attempts: u8,
    running: bool,
}

/// The lock distinguishes one profile startup from another editor using that profile.
pub(super) struct Startup {
    _lock: File,
    path: PathBuf,
    clear_at: Option<Instant>,
}

impl Startup {
    pub(super) fn arm(root: &Path) -> anyhow::Result<Option<(Self, bool)>> {
        fs::create_dir_all(root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("startup.lock"))?;
        if lock.try_lock().is_err() {
            return Ok(None);
        }
        let path = root.join("startup.json");
        let previous: Record = read_json(&path, 4096)?.unwrap_or_default();
        let attempts = if previous.running {
            previous.attempts.saturating_add(1)
        } else {
            1
        };
        let paused = attempts > 2;
        if !paused {
            write_json(
                &path,
                &Record {
                    attempts,
                    running: true,
                },
            )?;
        }
        let clear_at = (!paused).then(|| Instant::now() + Duration::from_secs(3));
        Ok(Some((
            Self {
                _lock: lock,
                path,
                clear_at,
            },
            paused,
        )))
    }

    pub(super) fn pending(&self) -> bool {
        self.clear_at.is_some()
    }

    pub(super) fn tick(&mut self) {
        if self
            .clear_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            let _ = self.clean();
        }
    }

    pub(super) fn clean(&mut self) -> anyhow::Result<()> {
        write_json(&self.path, &Record::default())?;
        self.clear_at = None;
        Ok(())
    }
}
