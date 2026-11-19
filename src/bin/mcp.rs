use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rustlabel::{Point, Workspace, preview, shape, shapes_mut};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Cursor, Read, Write},
    path::Path,
};

const MAX_LINE: u64 = 4 * 1024 * 1024;
const VERSION: &str = "2025-11-25";

#[derive(Deserialize)]
struct ImageArgs {
    image: String,
}

#[derive(Deserialize)]
struct ListArgs {
    #[serde(default)]
    offset: usize,
    #[serde(default = "page_size")]
    limit: usize,
    #[serde(default)]
    filter: String,
}

fn page_size() -> usize {
    100
}

#[derive(Deserialize)]
struct ShapeArgs {
    label: String,
    shape_type: String,
    points: Vec<Point>,
}

#[derive(Deserialize)]
struct EditArgs {
    image: String,
    revision: String,
    #[serde(default)]
    add: Vec<ShapeArgs>,
    #[serde(default)]
    update: Vec<Update>,
    #[serde(default)]
    remove: Vec<usize>,
    #[serde(default)]
    confirm_remove: bool,
}

#[derive(Deserialize)]
struct Update {
    index: usize,
    label: Option<String>,
    points: Option<Vec<Point>>,
}

struct Server {
    workspace: Workspace,
    write: bool,
    initialized: bool,
}

fn text(value: Value) -> Value {
    json!({"content":[{"type":"text","text":value.to_string()}]})
}

fn tools(write: bool) -> Vec<Value> {
    fn tool(
        name: &str,
        description: &str,
        props: Value,
        required: &[&str],
        read_only: bool,
    ) -> Value {
        json!({"name":name,"description":description,
            "inputSchema":{"type":"object","properties":props,"required":required},
            "annotations":{"readOnlyHint":read_only,"destructiveHint":!read_only,"openWorldHint":false}})
    }
    let str_type = json!({"type":"string"});
    let point_list = json!({"type":"array","items":{"type":"array","items":{"type":"number"},"minItems":2,"maxItems":2}});
    let idx = json!({"type":"integer","minimum":0,"description":"Zero-based shape index in the inspected revision"});
    let mut result = vec![
        tool(
            "list_images",
            "List relative image names from the workspace snapshot, optionally filter by substring.",
            json!({"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":500},"filter":str_type}),
            &[],
            true,
        ),
        tool(
            "inspect_image",
            "Read original pixel dimensions, Labelme shapes and the revision required to edit. Embedded image data is omitted.",
            json!({"image":str_type}),
            &["image"],
            true,
        ),
        tool(
            "preview_image",
            "Retrieve a scaled JPEG for visual inspection, plus preview and original dimensions. Use original image pixels when editing.",
            json!({"image":str_type}),
            &["image"],
            true,
        ),
    ];
    if write {
        result.push(tool("edit_annotations", "Atomically add, relabel, adjust, or remove annotations in one image. Inspect first. Updates preserve shape metadata, and all indices refer to the inspected revision. Removal requires explicit confirmation.",
            json!({
                "image":str_type,"revision":str_type,
                "add":{"type":"array","items":{"type":"object","required":["label","shape_type","points"],
                    "properties":{"label":str_type,"shape_type":{"enum":rustlabel::SHAPE_TYPES},"points":point_list}}},
                "update":{"type":"array","items":{"type":"object","required":["index"],
                    "properties":{"index":idx,"label":str_type,"points":point_list}}},
                "remove":{"type":"array","uniqueItems":true,"items":idx},
                "confirm_remove":{"type":"boolean"}
            }), &["image","revision"], false));
    }
    result
}

impl Server {
    fn call(&self, name: &str, args: Value) -> Result<Value> {
        match name {
            "list_images" => {
                let args: ListArgs = serde_json::from_value(args)?;
                ensure!(
                    (1..=500).contains(&args.limit),
                    "Limit must be between 1 and 500"
                );
                let filter = args.filter.to_lowercase();
                let mut entries = Vec::new();
                let mut total = 0usize;
                for entry in self.workspace.images()? {
                    if !filter.is_empty() && !entry.to_lowercase().contains(&filter) {
                        continue;
                    }
                    if total >= args.offset && entries.len() < args.limit {
                        entries.push(entry);
                    }
                    total += 1;
                }
                let next = args.offset.saturating_add(entries.len());
                Ok(text(
                    json!({"images":entries,"total":total,"next_offset":(next < total).then_some(next)}),
                ))
            }
            "inspect_image" => {
                let args: ImageArgs = serde_json::from_value(args)?;
                let mut snapshot = self.workspace.inspect(&args.image)?;
                snapshot
                    .doc
                    .as_object_mut()
                    .context("Invalid JSON object")?
                    .remove("imageData");
                Ok(text(
                    json!({"size":snapshot.size,"revision":snapshot.revision,"annotation":snapshot.doc}),
                ))
            }
            "preview_image" => {
                let args: ImageArgs = serde_json::from_value(args)?;
                let path = self.workspace.image_path(&args.image)?;
                let size = rustlabel::image_size(&path)?;
                let preview = preview(&path, 1280)?;
                let mut bytes = Vec::new();
                image::DynamicImage::ImageRgb8(preview.to_rgb8())
                    .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)?;
                Ok(json!({"content":[
                    {"type":"text","text":json!({"size":size,"preview_size":[preview.width(),preview.height()]}).to_string()},
                    {"type":"image","mimeType":"image/jpeg","data":STANDARD.encode(bytes)}
                ]}))
            }
            "edit_annotations" if self.write => {
                let args: EditArgs = serde_json::from_value(args)?;
                ensure!(
                    args.remove.is_empty() || args.confirm_remove,
                    "Removal requires confirm_remove=true"
                );
                ensure!(
                    !args.add.is_empty() || !args.update.is_empty() || !args.remove.is_empty(),
                    "No edits requested"
                );
                let mut snapshot = self.workspace.inspect(&args.image)?;
                ensure!(
                    snapshot.revision == args.revision,
                    "Annotation changed on disk. Inspect again."
                );
                let shapes = shapes_mut(&mut snapshot.doc)?;
                let mut touched = std::collections::HashSet::new();
                for update in args.update {
                    ensure!(touched.insert(update.index), "Duplicate update index");
                    let current = shapes
                        .get_mut(update.index)
                        .context("Update index does not exist")?;
                    ensure!(current.is_object(), "Invalid shape object");
                    if let Some(label) = update.label {
                        current["label"] =
                            shape(&label, "point", &[[0., 0.]], snapshot.size)?["label"].clone();
                    }
                    if let Some(points) = update.points {
                        let kind = current["shape_type"]
                            .as_str()
                            .context("Missing shape type")?;
                        current["points"] =
                            shape("validated", kind, &points, snapshot.size)?["points"].clone();
                    }
                }
                let mut remove = args.remove;
                remove.sort_unstable();
                for idx in remove.into_iter().rev() {
                    ensure!(
                        touched.insert(idx) && idx < shapes.len(),
                        "Invalid or overlapping removal index"
                    );
                    shapes.remove(idx);
                }
                for add in args.add {
                    shapes.push(shape(
                        &add.label,
                        &add.shape_type,
                        &add.points,
                        snapshot.size,
                    )?);
                }
                let count = shapes.len();
                let revision = self
                    .workspace
                    .save(&args.image, &args.revision, &snapshot.doc)?;
                Ok(text(json!({"revision":revision,"shape_count":count})))
            }
            _ => bail!("Tool unavailable"),
        }
    }

    fn rpc(&mut self, message: Value) -> Option<Value> {
        let id = message.get("id").cloned();
        let error = |id: Value, code, message| json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}});
        let method = message.get("method").and_then(Value::as_str);
        if message["jsonrpc"] != "2.0"
            || method.is_none()
            || id
                .as_ref()
                .is_some_and(|id| !id.is_string() && !id.is_number())
        {
            return Some(error(Value::Null, -32600, "Invalid request"));
        }
        let id = id?;
        let result = match method.unwrap() {
            "initialize" => {
                let Some(requested) = message["params"]["protocolVersion"].as_str() else {
                    return Some(error(id, -32602, "Expected protocolVersion"));
                };
                let version = match requested {
                    "2024-11-05" | "2025-03-26" | "2025-06-18" | VERSION => requested,
                    _ => VERSION,
                };
                self.initialized = true;
                json!({"protocolVersion":version,"serverInfo":{"name":"rustlabel","version":env!("CARGO_PKG_VERSION")},
                    "capabilities":{"tools":{}}})
            }
            "ping" => json!({}),
            _ if !self.initialized => return Some(error(id, -32002, "Initialize first")),
            "tools/list" => json!({"tools":tools(self.write)}),
            "tools/call" => {
                let params = &message["params"];
                let Some(name) = params["name"].as_str() else {
                    return Some(error(id, -32602, "Expected tool name"));
                };
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match self.call(name, args) {
                    Ok(result) => result,
                    Err(e) => {
                        json!({"isError":true,"content":[{"type":"text","text":e.to_string()}]})
                    }
                }
            }
            _ => return Some(error(id, -32601, "Method not found")),
        };
        Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let root = args
        .next()
        .context("Usage: rustlabel-mcp <workspace> [--write]")?;
    let write = match args.next() {
        None => false,
        Some(flag) if flag == "--write" && args.next().is_none() => true,
        _ => bail!("Usage: rustlabel-mcp <workspace> [--write]"),
    };
    let mut server = Server {
        workspace: Workspace::open(Path::new(&root))?,
        write,
        initialized: false,
    };
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut line = Vec::new();
    loop {
        line.clear();
        if (&mut input)
            .take(MAX_LINE + 1)
            .read_until(b'\n', &mut line)?
            == 0
        {
            break;
        }
        ensure!(
            line.len() as u64 <= MAX_LINE,
            "MCP request exceeds the input limit"
        );
        let response = match serde_json::from_slice(&line) {
            Ok(message) => server.rpc(message),
            Err(_) => Some(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
            ),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut output, &response)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(())
}
