use rustlabel::{IndexedAnnotationLabels, Workspace};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Deserialize, Serialize)]
struct CacheTime {
    secs_since_epoch: u64,
    nanos_since_epoch: u32,
    #[serde(default)]
    before_epoch: bool,
}

impl CacheTime {
    fn new(time: SystemTime) -> Self {
        let (duration, before_epoch) = match time.duration_since(UNIX_EPOCH) {
            Ok(duration) => (duration, false),
            Err(error) => (error.duration(), true),
        };
        Self {
            secs_since_epoch: duration.as_secs(),
            nanos_since_epoch: duration.subsec_nanos(),
            before_epoch,
        }
    }

    fn system_time(&self) -> Option<SystemTime> {
        if self.nanos_since_epoch >= 1_000_000_000 {
            return None;
        }
        let duration = Duration::new(self.secs_since_epoch, self.nanos_since_epoch);
        if self.before_epoch {
            UNIX_EPOCH.checked_sub(duration)
        } else {
            UNIX_EPOCH.checked_add(duration)
        }
    }
}

#[derive(Deserialize, Serialize)]
struct Entry {
    stamp: (CacheTime, u64),
    labels: BTreeSet<String>,
}

#[derive(Deserialize)]
pub(super) struct WorkspaceCache {
    pub images: Vec<String>,
    pub classes: BTreeSet<String>,
    pub skipped: usize,
    #[serde(default)]
    pub next_image: Option<usize>,
}

impl WorkspaceCache {
    pub fn load(session_path: Option<&Path>, root: &Path, selected_files: bool) -> Option<Self> {
        let path = Self::path(session_path?, root, selected_files)?;
        serde_json::from_slice(&fs::read(path).ok()?).ok()
    }

    pub fn save(
        session_path: &Path,
        workspace: &Workspace,
        classes: &BTreeSet<String>,
        skipped: usize,
        next_image: Option<usize>,
    ) -> anyhow::Result<()> {
        #[derive(Serialize)]
        struct Cache<'a> {
            images: &'a [String],
            classes: &'a BTreeSet<String>,
            skipped: usize,
            #[serde(skip_serializing_if = "Option::is_none")]
            next_image: Option<usize>,
        }

        let Some(path) = Self::path(
            session_path,
            workspace.root(),
            workspace.is_file_selection(),
        ) else {
            return Ok(());
        };
        let parent = path.parent().expect("workspace cache directory");
        fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        let mut writer = BufWriter::new(&mut temp);
        serde_json::to_writer(
            &mut writer,
            &Cache {
                images: workspace.images()?,
                classes,
                skipped,
                next_image,
            },
        )?;
        writer.flush()?;
        drop(writer);
        temp.persist(&path)?;
        Ok(())
    }

    fn path(session_path: &Path, root: &Path, selected_files: bool) -> Option<PathBuf> {
        let key = Sha256::digest(root.as_os_str().as_encoded_bytes());
        let suffix = if selected_files {
            "selection"
        } else {
            "folder"
        };
        Some(
            session_path
                .parent()?
                .join("workspace-cache-v1")
                .join(format!("{key:x}-{suffix}.json")),
        )
    }
}

pub(super) struct ClassCache {
    path: Option<PathBuf>,
    entries: BTreeMap<String, Entry>,
    changed: bool,
}

impl ClassCache {
    pub fn load(session_path: Option<&Path>, workspace: &Workspace) -> Self {
        let path = session_path.and_then(Path::parent).map(|parent| {
            let key = Sha256::digest(workspace.root().as_os_str().as_encoded_bytes());
            parent.join("class-cache-v1").join(format!("{key:x}.json"))
        });
        let entries = path
            .as_ref()
            .and_then(|path| File::open(path).ok())
            .and_then(|file| serde_json::from_reader(BufReader::new(file)).ok())
            .unwrap_or_default();
        Self {
            path,
            entries,
            changed: false,
        }
    }

    pub fn preview_labels(&self, images: &[String]) -> BTreeSet<String> {
        self.entries
            .iter()
            .filter(|(name, _)| images.binary_search(name).is_ok())
            .flat_map(|(_, entry)| entry.labels.iter().cloned())
            .collect()
    }

    pub fn collect(
        &mut self,
        workspace: &Workspace,
        image: &str,
        labels: &mut BTreeSet<String>,
    ) -> anyhow::Result<()> {
        let cached = self.entries.get(image);
        let stamp = cached.and_then(|entry| {
            entry
                .stamp
                .0
                .system_time()
                .map(|modified| (modified, entry.stamp.1))
        });
        match workspace.indexed_annotation_labels(image, stamp) {
            Ok(IndexedAnnotationLabels::Unchanged) => {
                if let Some(entry) = cached {
                    labels.extend(entry.labels.iter().cloned());
                }
            }
            Ok(IndexedAnnotationLabels::Read {
                stamp,
                labels: found,
            }) => {
                let found: BTreeSet<_> = found.into_iter().collect();
                labels.extend(found.iter().cloned());
                self.entries.insert(
                    image.to_owned(),
                    Entry {
                        stamp: (CacheTime::new(stamp.0), stamp.1),
                        labels: found,
                    },
                );
                self.changed = true;
            }
            result => {
                self.changed |= self.entries.remove(image).is_some();
                result?;
            }
        }
        Ok(())
    }

    pub fn save(mut self, workspace: &Workspace, images: &[String]) -> anyhow::Result<()> {
        if !workspace.is_file_selection() {
            let before = self.entries.len();
            self.entries
                .retain(|name, _| images.binary_search(name).is_ok());
            self.changed |= self.entries.len() != before;
        }
        if self.changed
            && let Some(path) = self.path
            && let Some(parent) = path.parent()
        {
            fs::create_dir_all(parent)?;
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            let mut writer = BufWriter::new(&mut temp);
            serde_json::to_writer(&mut writer, &self.entries)?;
            writer.flush()?;
            drop(writer);
            temp.persist(path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_survives_reload_and_updates_changed_added_and_removed_sidecars() -> anyhow::Result<()>
    {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("dataset");
        fs::create_dir(&root)?;
        for name in ["keep", "edit", "removed", "new"] {
            fs::write(root.join(format!("{name}.png")), [])?;
        }
        let annotate = |name: &str, label: &str| {
            fs::write(
                root.join(format!("{name}.json")),
                serde_json::json!({"shapes": [{"label": label}]}).to_string(),
            )
        };
        annotate("keep", "retained")?;
        annotate("edit", "old")?;
        annotate("removed", "deleted")?;
        let session = dir.path().join("session.json");
        let workspace = Workspace::open(&root)?;
        let scan = || -> anyhow::Result<(ClassCache, BTreeSet<String>)> {
            let mut cache = ClassCache::load(Some(&session), &workspace);
            let mut labels = BTreeSet::new();
            for image in workspace.images()? {
                cache.collect(&workspace, image, &mut labels)?;
            }
            Ok((cache, labels))
        };
        let (cold, labels) = scan()?;
        assert!(cold.changed);
        assert_eq!(
            labels,
            BTreeSet::from_iter(["deleted", "old", "retained"].map(str::to_owned))
        );
        cold.save(&workspace, workspace.images()?)?;
        let (warm, cached_labels) = scan()?;
        assert!(!warm.changed);
        assert_eq!(cached_labels, labels);
        assert_eq!(warm.preview_labels(workspace.images()?), labels);
        assert_eq!(
            warm.preview_labels(&["keep.png".into()]),
            BTreeSet::from(["retained".into()])
        );

        annotate("edit", "updated")?;
        annotate("new", "added")?;
        fs::remove_file(root.join("removed.json"))?;
        let (changed, labels) = scan()?;
        assert!(changed.changed);
        assert_eq!(
            labels,
            BTreeSet::from_iter(["added", "retained", "updated"].map(str::to_owned))
        );
        changed.save(&workspace, workspace.images()?)?;
        assert!(!scan()?.0.changed);
        Ok(())
    }

    #[test]
    fn cached_times_round_trip_on_both_sides_of_the_unix_epoch() -> anyhow::Result<()> {
        for time in [
            UNIX_EPOCH - Duration::from_millis(1500),
            UNIX_EPOCH + Duration::from_millis(1500),
        ] {
            let bytes = serde_json::to_vec(&CacheTime::new(time))?;
            let cached: CacheTime = serde_json::from_slice(&bytes)?;
            assert_eq!(cached.system_time(), Some(time));
        }
        let old: CacheTime =
            serde_json::from_str(r#"{"secs_since_epoch":1,"nanos_since_epoch":500000000}"#)?;
        assert_eq!(
            old.system_time(),
            Some(UNIX_EPOCH + Duration::from_millis(1500))
        );
        Ok(())
    }
}
