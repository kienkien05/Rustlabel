use image::{ImageBuffer, Rgba};
use rustlabel::{Workspace, shape, shapes_mut};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Barrier},
};

fn sample(root: &Path) {
    ImageBuffer::from_pixel(96, 64, Rgba([25u8, 120, 100, 255]))
        .save(root.join("sample.png"))
        .unwrap();
}

#[test]
fn save_preserves_metadata_and_detects_stale_writes() {
    let dir = tempfile::tempdir().unwrap();
    sample(dir.path());
    let workspace = Workspace::open(dir.path()).unwrap();
    let mut initial = workspace.inspect("sample.png").unwrap();
    assert_eq!(initial.size, [96, 64]);
    assert_eq!(initial.revision, "missing");
    initial.doc["source_metadata"] = json!({"dataset":"original"});
    let mut rect = shape("car", "rectangle", &[[70., 50.], [15., 8.]], initial.size).unwrap();
    rect["description"] = json!("source label");
    shapes_mut(&mut initial.doc).unwrap().push(rect);
    let revision = workspace
        .save("sample.png", "missing", &initial.doc)
        .unwrap();
    let old_bytes = fs::read(dir.path().join("sample.json")).unwrap();
    let mut snapshot = workspace.inspect("sample.png").unwrap();
    assert_eq!(snapshot.revision, revision);
    assert_eq!(
        snapshot.doc["shapes"][0]["points"],
        json!([[15., 8.], [70., 50.]])
    );
    snapshot.doc["shapes"][0]["label"] = json!("truck");
    workspace
        .save("sample.png", &revision, &snapshot.doc)
        .unwrap();
    assert!(
        workspace
            .save("sample.png", &revision, &initial.doc)
            .is_err()
    );
    assert_eq!(workspace.inspect("sample.png").unwrap().doc, snapshot.doc);
    let backup = fs::read_dir(dir.path().join(".rustlabel/history"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(fs::read(backup.path()).unwrap(), old_bytes);
}

#[test]
fn parallel_writers_have_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    sample(dir.path());
    let workspace = Workspace::open(dir.path()).unwrap();
    let baseline = workspace.inspect("sample.png").unwrap();
    let start = Arc::new(Barrier::new(2));
    let handles: Vec<_> = ["car", "bus"]
        .into_iter()
        .map(|label| {
            let workspace = Workspace::open(dir.path()).unwrap();
            let start = start.clone();
            let mut doc = baseline.doc.clone();
            shapes_mut(&mut doc)
                .unwrap()
                .push(shape(label, "point", &[[9., 9.]], baseline.size).unwrap());
            std::thread::spawn(move || {
                start.wait();
                workspace.save("sample.png", "missing", &doc)
            })
        })
        .collect();
    let wins = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(Result::is_ok)
        .count();
    assert_eq!(wins, 1);
}

#[test]
fn unsafe_paths_and_collisions_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    sample(dir.path());
    let workspace = Workspace::open(dir.path()).unwrap();
    for path in [
        "../sample.png",
        "C:/sample.png",
        "folder/../../sample.png",
        "sample.png:stream.png",
        ".rustlabel/sample.png",
    ] {
        assert!(workspace.image_path(path).is_err(), "{path}");
    }
    fs::copy(dir.path().join("sample.png"), dir.path().join("sample.bmp")).unwrap();
    assert!(workspace.inspect("sample.png").is_err());
}

#[cfg(unix)]
#[test]
fn symlink_annotations_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    sample(dir.path());
    std::os::unix::fs::symlink(outside.path(), dir.path().join("sample.json")).unwrap();
    assert!(
        Workspace::open(dir.path())
            .unwrap()
            .inspect("sample.png")
            .is_err()
    );
}

#[test]
fn geometry_rejects_out_of_bounds_and_degenerate_shapes() {
    assert!(shape("car", "rectangle", &[[0., 0.], [101., 80.]], [100, 100]).is_err());
    assert!(shape("car", "rectangle", &[[1., 1.], [1., 3.]], [100, 100]).is_err());
    assert!(
        shape(
            "car",
            "polygon",
            &[[0., 0.], [1., 1.], [2., 2.]],
            [100, 100]
        )
        .is_err()
    );
    assert!(shape("", "point", &[[0., 0.]], [100, 100]).is_err());
}

#[test]
fn labelme_circle_line_and_linestrip_validate_geometry() {
    assert!(shape("rim", "circle", &[[20., 20.], [30., 20.]], [80, 60]).is_ok());
    assert!(shape("rim", "circle", &[[8., 8.], [20., 8.]], [80, 60]).is_err());
    assert!(shape("lane", "line", &[[10., 10.], [20., 10.]], [80, 60]).is_ok());
    assert!(shape("lane", "line", &[[10., 10.], [10., 10.]], [80, 60]).is_err());
    assert!(
        shape(
            "lane",
            "linestrip",
            &[[10., 10.], [40., 10.], [50., 20.]],
            [80, 60]
        )
        .is_ok()
    );
    assert!(shape("lane", "linestrip", &[[10., 10.]], [80, 60]).is_err());
}

fn session(root: &Path, write: bool, run: impl FnOnce(&mut dyn FnMut(&str, Value) -> Value)) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rustlabel-mcp"));
    command.arg(root);
    if write {
        command.arg("--write");
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut id = 0;
    let mut call = |method: &str, params: Value| {
        id += 1;
        writeln!(
            input,
            "{}",
            json!({"id":id,"jsonrpc":"2.0","method":method,"params":params})
        )
        .unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], id);
        response["result"].clone()
    };
    assert_eq!(
        call(
            "initialize",
            json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}})
        )["protocolVersion"],
        "2025-11-25"
    );
    run(&mut call);
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[test]
fn mcp_preview_edit_and_read_only_workflow() {
    let dir = tempfile::tempdir().unwrap();
    sample(dir.path());
    session(dir.path(), false, |call| {
        assert_eq!(
            call("tools/list", json!({}))["tools"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            call(
                "tools/call",
                json!({"name":"edit_annotations","arguments":{}})
            )["isError"],
            true
        );
    });
    session(dir.path(), true, |call| {
        assert_eq!(
            call("tools/list", json!({}))["tools"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
        let list = call("tools/call", json!({"name":"list_images","arguments":{}}));
        assert!(
            list["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("sample.png")
        );
        let preview = call(
            "tools/call",
            json!({"name":"preview_image","arguments":{"image":"sample.png"}}),
        );
        assert_eq!(preview["content"][1]["mimeType"], "image/jpeg");
        assert!(
            preview["content"][1]["data"]
                .as_str()
                .unwrap()
                .starts_with("/9j/")
        );
        let edit = json!({"name":"edit_annotations","arguments":{"image":"sample.png","revision":"missing",
            "add":[{"label":"car","shape_type":"rectangle","points":[[2,3],[18,28]]}]}});
        let result = call("tools/call", edit.clone());
        let saved: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(saved["shape_count"], 1);
        assert_eq!(call("tools/call", edit)["isError"], true);
        let bad_remove = json!({"name":"edit_annotations","arguments":{"image":"sample.png","revision":saved["revision"],"remove":[0]}});
        assert_eq!(call("tools/call", bad_remove)["isError"], true);
        let inspect = call(
            "tools/call",
            json!({"name":"inspect_image","arguments":{"image":"sample.png"}}),
        );
        let inspected: Value =
            serde_json::from_str(inspect["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(inspected["annotation"]["shapes"][0]["label"], "car");
        assert!(inspected["annotation"].get("imageData").is_none());
    });
}
