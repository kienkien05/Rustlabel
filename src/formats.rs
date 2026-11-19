use crate::{Point, Workspace, image_size, shape, shapes};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};
use tempfile::{Builder, TempDir};

#[derive(Clone, Copy)]
pub enum ExportFormat {
    Coco,
    YoloBoxes,
    YoloPolygons,
}

#[derive(Deserialize)]
struct Coco {
    images: Vec<CocoImage>,
    categories: Vec<Category>,
    annotations: Vec<CocoAnnotation>,
}

#[derive(Deserialize)]
struct CocoImage {
    id: u64,
    file_name: String,
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
struct Category {
    id: u64,
    name: String,
}

#[derive(Deserialize)]
struct CocoAnnotation {
    image_id: u64,
    category_id: u64,
    bbox: Option<[f64; 4]>,
    #[serde(default)]
    segmentation: Value,
    #[serde(default)]
    iscrowd: u8,
    #[serde(default)]
    keypoints: Vec<f64>,
}

fn read_text(path: &Path, max: u64) -> Result<String> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "Not a regular file: {}",
        path.display()
    );
    let mut text = String::new();
    File::open(path)?.take(max + 1).read_to_string(&mut text)?;
    ensure!(
        text.len() as u64 <= max,
        "File is too large: {}",
        path.display()
    );
    Ok(text)
}

fn output(parent: &Path, source: &Workspace, prefix: &str) -> Result<TempDir> {
    let parent = parent.canonicalize()?;
    ensure!(
        source.is_file_selection() || !parent.starts_with(source.root()),
        "Choose an output folder outside the source dataset"
    );
    Ok(Builder::new().prefix(prefix).tempdir_in(parent)?)
}

fn link_image(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target.parent().context("Image has no parent folder")?)?;
    fs::hard_link(source, target).with_context(|| {
        format!(
            "Cannot link {}. Choose an output folder on the same drive with hard-link support",
            source.display()
        )
    })
}

fn exact_json(path: &Path, doc: &Value) -> Result<()> {
    let mut out = BufWriter::new(File::create_new(path)?);
    serde_json::to_writer(&mut out, doc)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

fn write_labelme(
    output: &Path,
    source: &Path,
    name: &str,
    size: [u32; 2],
    items: Vec<Value>,
) -> Result<()> {
    let image = output.join(name);
    link_image(source, &image)?;
    if !items.is_empty() {
        exact_json(&image.with_extension("json"), &json!({
            "version":"5.0.1","flags":{},"shapes":items,"imagePath":image.file_name().and_then(|n|n.to_str()),
            "imageData":null,"imageWidth":size[0],"imageHeight":size[1]
        })).with_context(|| format!("Cannot create sidecar for {name}"))?;
    }
    Ok(())
}

fn write_classes(root: &Path, labels: impl Iterator<Item = impl AsRef<str>>) -> Result<()> {
    let mut out = BufWriter::new(File::create_new(root.join("labels.txt"))?);
    for label in labels {
        writeln!(out, "{}", label.as_ref())?;
    }
    out.flush()?;
    Ok(())
}

fn check_names(names: &HashMap<u64, String>) -> Result<()> {
    let mut used = HashSet::new();
    for name in names.values() {
        ensure!(
            *name == name.trim() && used.insert(name),
            "Duplicate or ambiguous class name: {name}"
        );
        shape(name, "point", &[[0., 0.]], [1, 1])?;
    }
    Ok(())
}

fn polygon_points(coords: &[f64]) -> Result<Vec<Point>> {
    ensure!(
        coords.len() >= 6 && coords.len().is_multiple_of(2),
        "Invalid polygon coordinates"
    );
    Ok(coords.chunks_exact(2).map(|xy| [xy[0], xy[1]]).collect())
}

pub fn import_coco(manifest: &Path, image_root: &Path, parent: &Path) -> Result<PathBuf> {
    let workspace = Workspace::open(image_root)?;
    let source = File::open(manifest)?;
    ensure!(
        source.metadata()?.len() <= 512 * 1024 * 1024,
        "COCO manifest exceeds 512 MiB"
    );
    let Coco {
        images,
        categories,
        annotations,
    } = serde_json::from_reader(BufReader::new(source)).context("Invalid COCO manifest")?;
    ensure!(!images.is_empty(), "No COCO images");
    let mut names = HashMap::new();
    for category in categories {
        ensure!(
            names.insert(category.id, category.name).is_none(),
            "Duplicate COCO category ID"
        );
    }
    check_names(&names)?;
    let mut by_image: HashMap<_, Vec<_>> = HashMap::new();
    for item in annotations {
        by_image.entry(item.image_id).or_default().push(item);
    }
    let output = output(parent, &workspace, "RustLabel-COCO-")?;
    let mut seen_ids = HashSet::new();
    let mut paths = HashSet::new();
    for image in images {
        ensure!(seen_ids.insert(image.id), "Duplicate image ID {}", image.id);
        let name = image.file_name.replace('\\', "/");
        let source = workspace
            .image_path(&name)
            .with_context(|| format!("COCO image {name}"))?;
        let size = image_size(&source)?;
        ensure!(
            size == [image.width, image.height],
            "Wrong dimensions in COCO entry {name}"
        );
        let sidecar = Path::new(&name).with_extension("json");
        ensure!(
            paths.insert(sidecar.to_string_lossy().to_lowercase()),
            "COCO sidecar paths overlap: {name}"
        );
        let items = by_image
            .remove(&image.id)
            .unwrap_or_default()
            .into_iter()
            .map(|item| {
                let label = names
                    .get(&item.category_id)
                    .context("Unknown COCO category ID")?;
                ensure!(
                    item.iscrowd == 0 && item.keypoints.is_empty(),
                    "COCO crowd masks and keypoints are not supported"
                );
                let bbox = || -> Result<_> {
                    let [x, y, w, h] = item.bbox.context("Missing COCO bounding box")?;
                    ensure!(w > 0. && h > 0., "Invalid COCO bounding box");
                    shape(label, "rectangle", &[[x, y], [x + w, y + h]], size)
                };
                match &item.segmentation {
                    Value::Null => bbox(),
                    Value::Array(polygons) if polygons.is_empty() => bbox(),
                    Value::Array(polygons) if polygons.len() == 1 => {
                        let coords = serde_json::from_value::<Vec<f64>>(polygons[0].clone())?;
                        shape(label, "polygon", &polygon_points(&coords)?, size)
                    }
                    _ => bail!("COCO bitmap masks and multi-part polygons are not supported"),
                }
            })
            .collect::<Result<Vec<_>>>()
            .with_context(|| format!("COCO labels for {name}"))?;
        write_labelme(output.path(), &source, &name, size, items)?;
    }
    ensure!(
        by_image.is_empty(),
        "Annotation references an unknown COCO image ID"
    );
    let ordered: BTreeMap<_, _> = names.into_iter().collect();
    write_classes(output.path(), ordered.values())?;
    Ok(output.keep())
}

#[derive(Deserialize)]
struct YoloYaml {
    names: YoloNames,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum YoloNames {
    List(Vec<String>),
    Indexed(HashMap<u64, String>),
}

fn yolo_names(root: &Path) -> Result<Option<HashMap<u64, String>>> {
    for name in ["data.yaml", "data.yml", "dataset.yaml", "dataset.yml"] {
        let path = root.join(name);
        if path.is_file() {
            let text = read_text(&path, 512 * 1024)?;
            let config: YoloYaml =
                serde_yaml_ng::from_str(&text).context("Invalid YOLO names in dataset YAML")?;
            let names = match config.names {
                YoloNames::List(names) => names
                    .into_iter()
                    .enumerate()
                    .map(|(i, n)| (i as u64, n))
                    .collect(),
                YoloNames::Indexed(names) => names,
            };
            check_names(&names)?;
            return Ok(Some(names));
        }
    }
    for name in ["classes.txt", "obj.names"] {
        let path = root.join(name);
        if path.is_file() {
            let text = read_text(&path, 512 * 1024)?;
            let names = text
                .trim_start_matches('\u{feff}')
                .lines()
                .map(str::trim)
                .enumerate()
                .map(|(i, n)| (i as u64, n.to_owned()))
                .collect();
            check_names(&names)?;
            return Ok(Some(names));
        }
    }
    Ok(None)
}

fn yolo_paths(name: &str) -> Option<(String, PathBuf)> {
    let mut chunks: Vec<_> = name.split('/').collect();
    let index = chunks.iter().position(|part| *part == "images")?;
    if index == chunks.len() - 1 {
        return None;
    }
    chunks[index] = "labels";
    let labels = PathBuf::from(chunks.join("/")).with_extension("txt");
    chunks.remove(index);
    Some((chunks.join("/"), labels))
}

fn yolo_rows(
    text: &str,
    names: Option<&HashMap<u64, String>>,
    size: [u32; 2],
) -> Result<Vec<Value>> {
    let w = f64::from(size[0]);
    let h = f64::from(size[1]);
    text.trim_start_matches('\u{feff}')
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            (|| {
                let mut fields = line.split_whitespace();
                let id = fields
                    .next()
                    .context("Missing YOLO class ID")?
                    .parse::<u64>()?;
                let label = match names {
                    Some(map) => map
                        .get(&id)
                        .with_context(|| format!("Unknown YOLO class ID {id}"))?
                        .clone(),
                    None => format!("class_{id}"),
                };
                let coords = fields
                    .map(str::parse::<f64>)
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                ensure!(
                    coords.iter().all(|v| (0. ..=1.).contains(v)),
                    "YOLO coordinates must be normalized"
                );
                match coords.as_slice() {
                    [x, y, bw, bh] => {
                        ensure!(*bw > 0. && *bh > 0., "YOLO box size must be positive");
                        let edges = [x - bw * 0.5, y - bh * 0.5, x + bw * 0.5, y + bh * 0.5];
                        ensure!(
                            edges.iter().all(|v| (-1e-6..=1.000001).contains(v)),
                            "YOLO box extends outside the image"
                        );
                        shape(
                            &label,
                            "rectangle",
                            &[
                                [edges[0].clamp(0., 1.) * w, edges[1].clamp(0., 1.) * h],
                                [edges[2].clamp(0., 1.) * w, edges[3].clamp(0., 1.) * h],
                            ],
                            size,
                        )
                    }
                    coords => {
                        let points = polygon_points(coords)?
                            .into_iter()
                            .map(|[x, y]| [x * w, y * h])
                            .collect::<Vec<_>>();
                        shape(&label, "polygon", &points, size)
                    }
                }
            })()
            .with_context(|| format!("YOLO label line {}", index + 1))
        })
        .collect()
}

pub fn import_yolo(dataset: &Path, parent: &Path) -> Result<PathBuf> {
    let workspace = Workspace::open(dataset)?;
    let names = yolo_names(workspace.root())?;
    let output = output(parent, &workspace, "RustLabel-YOLO-")?;
    let mut used = HashSet::new();
    let mut count = 0;
    let mut discovered = std::collections::BTreeSet::new();
    for name in workspace.images()? {
        let Some((target, labels)) = yolo_paths(name) else {
            continue;
        };
        ensure!(
            used.insert(
                Path::new(&target)
                    .with_extension("json")
                    .to_string_lossy()
                    .to_lowercase()
            ),
            "YOLO sidecar paths overlap: {name}"
        );
        let source = workspace.image_path(name)?;
        let size = image_size(&source)?;
        let labels = workspace.root().join(labels);
        let items = match fs::symlink_metadata(&labels) {
            Ok(_) => {
                ensure!(
                    labels.canonicalize()?.starts_with(workspace.root()),
                    "YOLO label path escapes dataset"
                );
                yolo_rows(&read_text(&labels, 16 * 1024 * 1024)?, names.as_ref(), size)
                    .with_context(|| format!("YOLO labels for {name}"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        discovered.extend(
            items
                .iter()
                .filter_map(|item| item["label"].as_str().map(str::to_owned)),
        );
        write_labelme(output.path(), &source, &target, size, items)?;
        count += 1;
    }
    ensure!(
        count > 0,
        "No images found inside an images/ folder in this dataset"
    );
    if let Some(names) = names {
        let ordered: BTreeMap<_, _> = names.into_iter().collect();
        write_classes(output.path(), ordered.values())?;
    } else {
        write_classes(output.path(), discovered.iter())?;
    }
    Ok(output.keep())
}

fn export_geometry(item: &Value, size: [u32; 2]) -> Result<(Vec<Point>, [f64; 4], f64, bool)> {
    let label = item["label"].as_str().context("Missing annotation label")?;
    let kind = item["shape_type"].as_str().unwrap_or_default();
    ensure!(
        matches!(kind, "rectangle" | "polygon"),
        "Cannot export shape type {kind}"
    );
    let points = serde_json::from_value::<Vec<Point>>(item["points"].clone())?;
    let checked = shape(label, kind, &points, size)?;
    let points = serde_json::from_value::<Vec<Point>>(checked["points"].clone())?;
    let w = points
        .iter()
        .map(|p| p[0])
        .fold(f64::NEG_INFINITY, f64::max);
    let h = points
        .iter()
        .map(|p| p[1])
        .fold(f64::NEG_INFINITY, f64::max);
    let x = points.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
    let y = points.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
    let polygon = kind == "polygon";
    let [w, h] = [w - x, h - y];
    let area = if polygon {
        (points
            .iter()
            .zip(points.iter().cycle().skip(1))
            .map(|(a, b)| a[0] * b[1] - b[0] * a[1])
            .sum::<f64>()
            / 2.)
            .abs()
    } else {
        w * h
    };
    Ok((points, [x, y, w, h], area, polygon))
}

pub fn export_dataset(
    workspace: &Workspace,
    parent: &Path,
    format: ExportFormat,
    classes: Vec<String>,
) -> Result<PathBuf> {
    let coco = matches!(format, ExportFormat::Coco);
    let output = output(
        parent,
        workspace,
        if coco {
            "RustLabel-export-COCO-"
        } else {
            "RustLabel-export-YOLO-"
        },
    )?;
    ensure!(!workspace.images()?.is_empty(), "No images to export");
    let mut class_ids = HashMap::new();
    for label in classes {
        let id = class_ids.len();
        class_ids.entry(label).or_insert(id);
    }
    let mut images = Vec::new();
    let mut annotations = Vec::new();
    for (index, name) in workspace.images()?.iter().enumerate() {
        let source = workspace.image_path(name)?;
        let image = output.path().join("images").join(name);
        link_image(&source, &image)?;
        let snapshot = workspace
            .inspect(name)
            .with_context(|| format!("Cannot load {name}"))?;
        let [width, height] = snapshot.size;
        let image_id = index + 1;
        let mut rows = Vec::new();
        let (w, h) = (f64::from(width), f64::from(height));
        if coco {
            images.push(json!({"id":image_id,"file_name":format!("images/{name}"),"width":width,"height":height}));
        }
        for item in shapes(&snapshot.doc)? {
            let (points, bbox, area, polygon) = export_geometry(item, snapshot.size)
                .with_context(|| format!("Cannot export labels for {name}"))?;
            let label = item["label"].as_str().context("Missing annotation label")?;
            let next = class_ids.len();
            let category = *class_ids.entry(label.to_owned()).or_insert(next);
            if coco {
                let segmentation = if polygon {
                    vec![points.into_iter().flatten().collect::<Vec<_>>()]
                } else {
                    vec![]
                };
                annotations.push(json!({"id":annotations.len()+1,"image_id":image_id,
                    "category_id":category+1,"bbox":bbox,"area":area,"iscrowd":0,"segmentation":segmentation}));
            } else if matches!(format, ExportFormat::YoloBoxes) {
                let [x, y, bw, bh] = bbox;
                rows.push(format!(
                    "{category} {:.10} {:.10} {:.10} {:.10}",
                    (x + bw * 0.5) / w,
                    (y + bh * 0.5) / h,
                    bw / w,
                    bh / h
                ));
            } else {
                let vertices = if polygon {
                    points
                } else {
                    let [x, y, bw, bh] = bbox;
                    vec![[x, y], [x + bw, y], [x + bw, y + bh], [x, y + bh]]
                };
                let mut row = category.to_string();
                for [x, y] in vertices {
                    row.push_str(&format!(" {:.10} {:.10}", x / w, y / h));
                }
                rows.push(row);
            }
        }
        if !coco {
            let path = output
                .path()
                .join("labels")
                .join(name)
                .with_extension("txt");
            fs::create_dir_all(path.parent().context("Missing YOLO labels directory")?)?;
            let mut file = File::create_new(path)?;
            if !rows.is_empty() {
                writeln!(file, "{}", rows.join("\n"))?;
            }
        }
    }
    let names: BTreeMap<_, _> = class_ids.into_iter().map(|(name, id)| (id, name)).collect();
    if coco {
        let categories: Vec<_> = names
            .into_iter()
            .map(|(id, name)| json!({"id":id+1,"name":name}))
            .collect();
        exact_json(
            &output.path().join("annotations.json"),
            &json!({"images":images,"annotations":annotations,"categories":categories}),
        )?;
    } else {
        let mut config = json!({"names":names.into_values().collect::<Vec<_>>()});
        if output.path().join("images/train").is_dir() {
            config["train"] = json!("images/train");
        }
        for dir in ["val", "valid"] {
            if output.path().join("images").join(dir).is_dir() {
                config["val"] = json!(format!("images/{dir}"));
                break;
            }
        }
        if output.path().join("images/test").is_dir() {
            config["test"] = json!("images/test");
        }
        let value = serde_yaml_ng::to_string(&config)?;
        fs::write(output.path().join("data.yaml"), value)?;
    }
    Ok(output.keep())
}
