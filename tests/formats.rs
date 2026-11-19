use image::RgbImage;
use rustlabel::{
    Workspace,
    formats::{ExportFormat, export_dataset, import_coco, import_yolo},
};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn image(root: &Path, name: &str) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    RgbImage::new(100, 80).save(path).unwrap();
}

#[test]
fn coco_box_polygon_and_unused_classes_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("dataset");
    image(&source, "train/a.png");
    image(&source, "valid/b.png");
    let manifest = source.join("instances.json");
    let original = json!({
        "images":[{"id":10,"file_name":"train/a.png","width":100,"height":80},
                  {"id":11,"file_name":"valid/b.png","width":100,"height":80}],
        "categories":[{"id":17,"name":"car"},{"id":31,"name":"never_used"}],
        "annotations":[
            {"image_id":10,"category_id":17,"bbox":[10.,20.,30.,20.]},
            {"image_id":10,"category_id":17,"segmentation":[[60.,10.,80.,10.,70.,30.]],"iscrowd":0}
        ]
    })
    .to_string();
    fs::write(&manifest, &original).unwrap();
    let work = import_coco(&manifest, &source, tmp.path()).unwrap();
    let workspace = Workspace::open(&work).unwrap();
    assert_eq!(
        workspace.annotation_labels("train/a.png").unwrap(),
        ["car", "car"]
    );
    assert_eq!(workspace.images().unwrap().len(), 2);
    assert!(
        fs::read_to_string(work.join("labels.txt"))
            .unwrap()
            .contains("never_used")
    );
    assert!(!source.join("train/a.json").exists());
    let saved = export_dataset(
        &workspace,
        tmp.path(),
        ExportFormat::Coco,
        vec!["never_used".into()],
    )
    .unwrap();
    let emitted: Value =
        serde_json::from_slice(&fs::read(saved.join("annotations.json")).unwrap()).unwrap();
    assert_eq!(
        emitted["annotations"][0]["bbox"],
        json!([10., 20., 30., 20.])
    );
    assert_eq!(
        emitted["annotations"][1]["segmentation"],
        json!([[60., 10., 80., 10., 70., 30.]])
    );
    assert_eq!(emitted["categories"].as_array().unwrap().len(), 2);
    assert_eq!(fs::read_to_string(manifest).unwrap(), original);
}

#[test]
fn yolo_normalized_geometry_roundtrips_with_split_and_names() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("dataset");
    image(&source, "train/images/a.png");
    image(&source, "valid/images/b.png");
    fs::create_dir_all(source.join("train/labels")).unwrap();
    fs::write(
        source.join("data.yaml"),
        "names:\n  0: car\n  1: road-sign\n  2: bus\n",
    )
    .unwrap();
    let original = "0 0.5 0.5 0.5 0.5\n1 0.1 0.1 0.3 0.1 0.2 0.3\n";
    fs::write(source.join("train/labels/a.txt"), original).unwrap();
    let work = import_yolo(&source, tmp.path()).unwrap();
    let workspace = Workspace::open(&work).unwrap();
    assert_eq!(workspace.images().unwrap(), ["train/a.png", "valid/b.png"]);
    assert_eq!(
        workspace.inspect("train/a.png").unwrap().doc["shapes"][0]["points"],
        json!([[25., 20.], [75., 60.]])
    );
    assert!(
        fs::read_to_string(work.join("labels.txt"))
            .unwrap()
            .contains("bus")
    );
    let classes = vec!["car".into(), "road-sign".into(), "bus".into()];
    for (format, kind) in [
        (ExportFormat::YoloBoxes, "rectangle"),
        (ExportFormat::YoloPolygons, "polygon"),
    ] {
        let output = export_dataset(&workspace, tmp.path(), format, classes.clone()).unwrap();
        let yaml = fs::read_to_string(output.join("data.yaml")).unwrap();
        assert!(yaml.contains("train: images/train") && yaml.contains("val: images/valid"));
        let imported = import_yolo(&output, tmp.path()).unwrap();
        let snapshot = Workspace::open(&imported)
            .unwrap()
            .inspect("train/a.png")
            .unwrap();
        assert_eq!(snapshot.doc["shapes"][0]["shape_type"], kind);
        assert_eq!(snapshot.doc["shapes"][1]["label"], "road-sign");
    }
    assert_eq!(
        fs::read_to_string(source.join("train/labels/a.txt")).unwrap(),
        original
    );
}

#[test]
fn unsupported_or_escaping_coco_input_never_publishes_partial_output() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("dataset");
    image(&source, "a.png");
    let parent = tmp.path().join("safe-output");
    fs::create_dir(&parent).unwrap();
    let manifest = source.join("instances.json");
    let mut document = json!({
        "images":[{"id":1,"file_name":"a.png","width":100,"height":80}],
        "categories":[{"id":1,"name":"car"}],
        "annotations":[{"image_id":1,"category_id":1,"bbox":[1,1,5,5],
                        "segmentation":{"counts":"mask","size":[80,100]},"iscrowd":1}]
    });
    fs::write(&manifest, document.to_string()).unwrap();
    assert!(
        import_coco(&manifest, &source, &parent)
            .unwrap_err()
            .to_string()
            .contains("COCO labels")
    );
    assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);
    document["images"][0]["file_name"] = json!("../a.png");
    assert!(import_coco(&manifest, &source, &source).is_err());
    fs::write(&manifest, document.to_string()).unwrap();
    assert!(import_coco(&manifest, &source, &parent).is_err());
    assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);
}
