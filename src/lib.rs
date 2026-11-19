use anyhow::{Context, Result, anyhow, bail, ensure};
use image::{DynamicImage, ImageReader};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, OnceLock},
    time::SystemTime,
};
use walkdir::WalkDir;

const PRIVATE: &str = ".rustlabel";
mod file_ops;
pub mod formats;
const MAX_JSON: u64 = 64 * 1024 * 1024;
pub type Point = [f64; 2];
pub const SHAPE_TYPES: [&str; 6] = [
    "rectangle",
    "polygon",
    "point",
    "circle",
    "line",
    "linestrip",
];

#[derive(Clone)]
pub struct Workspace {
    root: PathBuf,
    index: Arc<OnceLock<Index>>,
    selected_files: bool,
    cached_inventory: bool,
}

#[derive(Clone)]
struct Index {
    images: Vec<String>,
    collisions: HashSet<PathBuf>,
}

pub type Stamp = Option<(SystemTime, u64)>;

pub enum IndexedAnnotationLabels {
    Missing,
    Unchanged,
    Read { stamp: (SystemTime, u64), labels: Vec<String> },
}

pub struct Snapshot {
    pub doc: Value,
    pub revision: String,
    pub size: [u32; 2],
    pub stamp: Stamp,
}

impl Workspace {
    pub fn open(root: &Path) -> Result<Self> {
        let canonical = root.canonicalize().context("Cannot open workspace")?;
        ensure!(canonical.is_dir(), "Workspace must be a directory");
        Ok(Self {
            root: canonical,
            index: Arc::new(OnceLock::new()),
            selected_files: false,
            cached_inventory: false,
        })
    }

    pub fn open_files(paths: &[PathBuf]) -> Result<Self> {
        ensure!(!paths.is_empty(), "No images selected");
        let mut files = paths
            .iter()
            .map(|path| {
                let path = path
                    .canonicalize()
                    .with_context(|| format!("Cannot open {}", path.display()))?;
                ensure!(
                    path.is_file() && is_image(&path),
                    "Unsupported image: {}",
                    path.display()
                );
                Ok(path)
            })
            .collect::<Result<Vec<_>>>()?;
        files.sort_unstable();
        files.dedup();
        let mut root = files[0].parent().context("Missing image folder")?;
        while !files.iter().all(|file| file.starts_with(root)) {
            root = root.parent().context("Select images on the same drive")?;
            ensure!(
                !root.as_os_str().is_empty() && root.is_dir(),
                "Select images on the same drive"
            );
        }
        Self::file_selection(root, &files)
    }

    pub fn restore_file_selection(root: &Path, names: &[String]) -> Result<Self> {
        let workspace = Self::open(root)?;
        let files: Vec<_> = names
            .iter()
            .filter_map(|name| workspace.image_path(name).ok())
            .collect();
        Self::file_selection(&workspace.root, &files)
    }

    /// Restore a previously indexed list. Individual image accesses still validate their paths.
    pub fn with_cached_inventory(
        mut self,
        images: Vec<String>,
        selected_files: bool,
    ) -> Result<Self> {
        ensure!(
            images.windows(2).all(|pair| pair[0] < pair[1]),
            "Invalid cached image order"
        );
        let mut seen = HashSet::new();
        let mut collisions = HashSet::new();
        for name in &images {
            let relative = Path::new(name);
            ensure!(
                !name.contains(['\\', ':'])
                    && is_image(relative)
                    && relative.components().all(|part| {
                        matches!(part, Component::Normal(name) if !name.to_string_lossy().starts_with('.'))
                    }),
                "Invalid cached image path"
            );
            let key = sidecar_key(&self.root.join(relative));
            if !seen.insert(key.clone()) {
                collisions.insert(key);
            }
        }
        self.index = Arc::new(OnceLock::from(Index { images, collisions }));
        self.selected_files = selected_files;
        self.cached_inventory = true;
        Ok(self)
    }

    fn file_selection(root: &Path, files: &[PathBuf]) -> Result<Self> {
        let mut images = Vec::with_capacity(files.len());
        let mut seen = HashSet::new();
        let mut collisions = HashSet::new();
        for file in files {
            let relative = file.strip_prefix(root)?;
            ensure!(relative.components().all(|part| matches!(part,Component::Normal(name) if !name.to_string_lossy().starts_with('.'))),
                "Hidden or invalid image path");
            let name = relative
                .to_str()
                .context("Image path is not Unicode")?
                .replace(std::path::MAIN_SEPARATOR, "/");
            ensure!(!name.contains(['\\', ':']), "Invalid image path");
            let key = sidecar_key(file);
            if !seen.insert(key.clone()) {
                collisions.insert(key);
            }
            images.push(name);
        }
        images.sort_unstable();
        images.dedup();
        Ok(Self {
            root: root.to_path_buf(),
            index: Arc::new(OnceLock::from(Index { images, collisions })),
            selected_files: true,
            cached_inventory: false,
        })
    }

    pub fn is_file_selection(&self) -> bool {
        self.selected_files
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn index(&self) -> Result<&Index> {
        if let Some(index) = self.index.get() {
            return Ok(index);
        }
        let mut images = Vec::new();
        let mut seen = HashSet::new();
        let mut collisions = HashSet::new();
        for item in WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
        {
            let item = item?;
            if item.file_type().is_file() && is_image(item.path()) {
                let rel = item.path().strip_prefix(&self.root)?;
                if let Some(name) = rel.to_str() {
                    let name = name.replace(std::path::MAIN_SEPARATOR, "/");
                    if !name.contains(['\\', ':']) {
                        let key = sidecar_key(item.path());
                        if !seen.insert(key.clone()) {
                            collisions.insert(key);
                        }
                        images.push(name);
                    }
                }
            }
        }
        images.sort_unstable();
        let _ = self.index.set(Index { images, collisions });
        Ok(self.index.get().expect("Index initialized"))
    }

    pub fn images(&self) -> Result<&[String]> {
        Ok(&self.index()?.images)
    }

    fn sidecar(&self, image: &Path) -> Result<PathBuf> {
        ensure!(
            !self.index()?.collisions.contains(&sidecar_key(image)),
            "Two images share a Labelme sidecar name"
        );
        if self.selected_files || self.cached_inventory {
            let extension = image
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            ensure!(
                !IMAGE_EXTENSIONS
                    .iter()
                    .filter(|ext| !ext.eq_ignore_ascii_case(extension))
                    .any(|ext| image.with_extension(ext).is_file()
                        || image.with_extension(ext.to_ascii_uppercase()).is_file()),
                "Another image has the same Labelme sidecar name"
            );
        }
        Ok(image.with_extension("json"))
    }

    pub fn stamp(&self, name: &str) -> Result<Stamp> {
        annotation_stamp(&self.sidecar(&self.image_path(name)?)?)
    }

    /// Read labels only when a sidecar's metadata differs from the cached stamp.
    pub fn indexed_annotation_labels(
        &self,
        name: &str,
        cached: Stamp,
    ) -> Result<IndexedAnnotationLabels> {
        let index = self.index()?;
        ensure!(
            index.images
                .binary_search_by(|entry| entry.as_str().cmp(name))
                .is_ok(),
            "Image is not part of the workspace index"
        );
        let candidate = self.root.join(name).with_extension("json");
        let Some(stamp) = annotation_stamp(&candidate)? else {
            return Ok(IndexedAnnotationLabels::Missing);
        };
        if cached == Some(stamp)
            && !self.selected_files
            && !self.cached_inventory
            && !index.collisions.contains(&sidecar_key(&candidate))
        {
            return Ok(IndexedAnnotationLabels::Unchanged);
        }
        let sidecar = self.sidecar_path(name)?;
        let observed = if sidecar == candidate {
            Some(stamp)
        } else {
            annotation_stamp(&sidecar)?
        };
        if observed.is_some() && observed == cached {
            return Ok(IndexedAnnotationLabels::Unchanged);
        }
        match read_annotation(&sidecar)? {
            (Some(bytes), Some(stamp)) => Ok(IndexedAnnotationLabels::Read {
                stamp,
                labels: labels_from_json(&bytes)?,
            }),
            _ => Ok(IndexedAnnotationLabels::Missing),
        }
    }

    pub fn annotation_labels(&self, name: &str) -> Result<Vec<String>> {
        let sidecar = self.sidecar_path(name)?;
        let Some(bytes) = read_annotation(&sidecar)?.0 else {
            return Ok(Vec::new());
        };
        labels_from_json(&bytes)
    }

    pub fn sidecar_path(&self, name: &str) -> Result<PathBuf> {
        self.sidecar(&self.image_path(name)?)
    }

    pub fn delete_annotation(&self, name: &str) -> Result<bool> {
        Ok(self.archive_annotation(name)?.is_some())
    }

    pub fn archive_annotation(&self, name: &str) -> Result<Option<PathBuf>> {
        let sidecar = self.sidecar_path(name)?;
        let _lock = lock_sidecar(&sidecar)?;
        self.image_path(name)?;
        if annotation_stamp(&sidecar)?.is_none() {
            return Ok(None);
        }
        Ok(Some(file_ops::archive_files(&[&sidecar])?.keep()))
    }

    pub fn image_path(&self, name: &str) -> Result<PathBuf> {
        let relative = Path::new(name);
        ensure!(
            !name.contains(['\\', ':']) && is_image(relative),
            "Invalid image path"
        );
        if self.selected_files {
            ensure!(
                self.images()?
                    .binary_search_by(|entry| entry.as_str().cmp(name))
                    .is_ok(),
                "Image is not part of the file selection"
            );
        }
        let mut path = self.root.clone();
        for item in relative.components() {
            let Component::Normal(part) = item else {
                bail!("Expected a relative image path")
            };
            ensure!(
                !part.to_string_lossy().starts_with('.'),
                "Hidden paths are not accessible"
            );
            path.push(part);
            let resolved = path.canonicalize().context("Image path does not exist")?;
            ensure!(
                resolved.starts_with(&self.root),
                "Image escapes the workspace"
            );
            ensure!(
                !fs::symlink_metadata(&path)?.is_symlink(),
                "Image aliases are not supported"
            );
            path = resolved;
        }
        ensure!(path.is_file(), "Expected a regular image file");
        Ok(path)
    }

    pub fn inspect(&self, name: &str) -> Result<Snapshot> {
        let image = self.image_path(name)?;
        let size = image_size(&image)?;
        let sidecar = self.sidecar(&image)?;
        let (bytes, stamp) = read_annotation(&sidecar)?;
        let revision = revision_of(bytes.as_deref());
        let doc = match bytes {
            Some(bytes) => {
                let doc: Value = serde_json::from_slice(&bytes).context("Invalid Labelme JSON")?;
                shapes(&doc)?;
                ensure!(
                    doc.get("imagePath")
                        .and_then(Value::as_str)
                        .is_none_or(|p| p.rsplit(['/', '\\']).next().is_some_and(|base| image
                            .file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.eq_ignore_ascii_case(base)))),
                    "Sidecar belongs to another image"
                );
                doc
            }
            None => json!({
                "version": "5.0.1", "flags": {}, "shapes": [], "imagePath": image.file_name().and_then(|n| n.to_str()),
                "imageData": null, "imageWidth": size[0], "imageHeight": size[1]
            }),
        };
        Ok(Snapshot {
            doc,
            revision,
            size,
            stamp,
        })
    }

    pub fn save(&self, name: &str, expected: &str, doc: &Value) -> Result<String> {
        let image = self.image_path(name)?;
        let sidecar = self.sidecar(&image)?;
        shapes(doc)?;
        let image_folder = image.parent().context("Missing image folder")?;
        let lock_dir = checked_dir(image_folder, PRIVATE)?;
        let _lock = lock_sidecar(&sidecar)?;
        self.image_path(name)?;
        let (old, _) = read_annotation(&sidecar)?;
        ensure!(
            revision_of(old.as_deref()) == expected,
            "Annotation changed on disk. Reload and retry."
        );
        let mut bytes = serde_json::to_vec_pretty(doc)?;
        bytes.push(b'\n');
        ensure!(
            bytes.len() as u64 <= MAX_JSON,
            "Annotation exceeds the size limit"
        );
        if old.as_deref() == Some(&bytes) {
            return Ok(revision_of(Some(&bytes)));
        }
        if let Some(old) = old {
            let private = if self.selected_files {
                lock_dir
            } else {
                checked_dir(&self.root, PRIVATE)?
            };
            let history = checked_dir(&private, "history")?;
            let prefix = format!("{}-", digest(sidecar.to_string_lossy().as_bytes()));
            let mut backup = tempfile::Builder::new()
                .prefix(&prefix[..13])
                .suffix(".json")
                .tempfile_in(history)?;
            backup.write_all(&old)?;
            backup.as_file().sync_all()?;
            backup.keep()?;
        }
        let mut temporary =
            tempfile::NamedTempFile::new_in(sidecar.parent().context("Missing parent directory")?)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(sidecar)?;
        Ok(revision_of(Some(&bytes)))
    }
}

fn lock_sidecar(sidecar: &Path) -> Result<fs::File> {
    let folder = sidecar.parent().context("Missing image folder")?;
    let locks = checked_dir(&checked_dir(folder, PRIVATE)?, "locks")?;
    let lock_path = locks.join(format!(
        "{}.lock",
        digest(sidecar.to_string_lossy().to_lowercase().as_bytes())
    ));
    // Persist lock files so concurrent processes always lock the same inode.
    if let Ok(meta) = fs::symlink_metadata(&lock_path) {
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "Invalid lock file"
        );
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    fs2::FileExt::lock_exclusive(&lock)?;
    Ok(lock)
}

fn checked_dir(parent: &Path, name: &str) -> Result<PathBuf> {
    let path = parent.join(name);
    match fs::create_dir(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let info = fs::symlink_metadata(&path)?;
    ensure!(
        info.is_dir() && path.canonicalize()? == path,
        "Invalid workspace metadata directory"
    );
    Ok(path)
}

pub const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "bmp", "webp"];

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|extension| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|ext| ext.eq_ignore_ascii_case(extension))
        })
}

fn sidecar_key(image: &Path) -> PathBuf {
    PathBuf::from(
        image
            .with_extension("json")
            .to_string_lossy()
            .to_lowercase(),
    )
}

fn labels_from_json(bytes: &[u8]) -> Result<Vec<String>> {
    #[derive(Deserialize)]
    struct LabelSidecar {
        shapes: Vec<LabelOnly>,
    }

    #[derive(Deserialize)]
    struct LabelOnly {
        label: String,
    }

    let sidecar: LabelSidecar = serde_json::from_slice(bytes)?;
    Ok(sidecar.shapes.into_iter().map(|item| item.label).collect())
}

fn annotation_stamp(path: &Path) -> Result<Stamp> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "Invalid annotation file"
            );
            Ok(Some((meta.modified()?, meta.len())))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn read_annotation(path: &Path) -> Result<(Option<Vec<u8>>, Stamp)> {
    let Some(_) = annotation_stamp(path)? else {
        return Ok((None, None));
    };
    let file = fs::File::open(path)?;
    let meta = file.metadata()?;
    let mut bytes = Vec::new();
    file.take(MAX_JSON + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_JSON,
        "Annotation exceeds the size limit"
    );
    Ok((Some(bytes), Some((meta.modified()?, meta.len()))))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn revision_of(bytes: Option<&[u8]>) -> String {
    bytes.map_or_else(|| "missing".to_owned(), digest)
}

pub fn image_size(path: &Path) -> Result<[u32; 2]> {
    let (w, h) = ImageReader::open(path)?
        .with_guessed_format()?
        .into_dimensions()?;
    Ok([w, h])
}

pub fn preview(path: &Path, edge: u32) -> Result<DynamicImage> {
    let mut reader = ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?;
    if image.width().max(image.height()) <= edge {
        Ok(image)
    } else {
        Ok(image.thumbnail(edge, edge))
    }
}

pub fn shapes(doc: &Value) -> Result<&Vec<Value>> {
    doc.get("shapes")
        .and_then(Value::as_array)
        .context("Missing Labelme shapes array")
}

pub fn shapes_mut(doc: &mut Value) -> Result<&mut Vec<Value>> {
    doc.get_mut("shapes")
        .and_then(Value::as_array_mut)
        .context("Missing Labelme shapes array")
}

pub fn validate_label(label: &str) -> Result<&str> {
    let label = label.trim();
    ensure!(
        !label.is_empty() && label.chars().count() <= 256 && !label.chars().any(char::is_control),
        "Invalid label"
    );
    Ok(label)
}

pub fn shape(label: &str, kind: &str, points: &[Point], size: [u32; 2]) -> Result<Value> {
    let label = validate_label(label)?;
    ensure!(
        points.iter().all(|[x, y]| x.is_finite()
            && y.is_finite()
            && *x >= 0.0
            && *y >= 0.0
            && *x <= f64::from(size[0])
            && *y <= f64::from(size[1])),
        "Coordinates must be inside the image"
    );
    let points = match (kind, points) {
        ("rectangle", [a, b]) if a[0] != b[0] && a[1] != b[1] => vec![
            [a[0].min(b[0]), a[1].min(b[1])],
            [a[0].max(b[0]), a[1].max(b[1])],
        ],
        ("polygon", p) if (3..=10000).contains(&p.len()) => {
            let area: f64 = p
                .iter()
                .zip(p.iter().cycle().skip(1))
                .map(|(a, b)| a[0] * b[1] - b[0] * a[1])
                .sum();
            ensure!(area.abs() > 1e-8, "Polygon has zero area");
            p.to_vec()
        }
        ("circle", [center, edge]) if center != edge => {
            let radius = (center[0] - edge[0]).hypot(center[1] - edge[1]);
            let limit = center[0]
                .min(center[1])
                .min(f64::from(size[0]) - center[0])
                .min(f64::from(size[1]) - center[1]);
            ensure!(radius <= limit + 1e-6, "Circle extends beyond the image");
            points.to_vec()
        }
        ("line", [a, b]) if a != b => points.to_vec(),
        ("linestrip", p)
            if (2..=10000).contains(&p.len()) && p.windows(2).any(|pair| pair[0] != pair[1]) =>
        {
            p.to_vec()
        }
        ("point", [p]) => vec![*p],
        _ => return Err(anyhow!("Invalid geometry for {kind}")),
    };
    Ok(
        json!({"label":label,"points":points,"shape_type":kind,"group_id":null,"flags":{},"description":""}),
    )
}
