# RustLabel

![RustLabel logo](assets/rustlabel-logo.png)

Native desktop image annotation for Windows, Linux, and macOS, with a separate local MCP executable. Both use the same Labelme JSON sidecars. Existing images are never modified.

## Setup and build

### Windows (PowerShell)

```powershell
winget install --exact --id Git.Git
winget install --exact --id Microsoft.VisualStudio.2022.BuildTools --override '--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
winget install --exact --id Rustlang.Rustup
```

Open a new PowerShell window after setup so Git and Cargo are on your PATH. The C++ workload installs the MSVC toolchain and Windows SDK. [Visual Studio Build Tools components](https://learn.microsoft.com/en-us/visualstudio/install/workload-component-id-vs-build-tools)

### Linux (Debian or Ubuntu)

```sh
sudo apt-get update
sudo apt-get install -y git curl build-essential pkg-config libxkbcommon-dev \
    libwayland-dev libgl1-mesa-dev libx11-dev libxcursor-dev libxi-dev libxrandr-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
```

The folder picker uses an XDG desktop portal. You can also pass the image folder on the command line.

### macOS

```sh
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
```

Complete the Xcode Command Line Tools installation before building. [Rust installation](https://rust-lang.org/tools/install/)

### Clone and build on any platform

```sh
git clone https://github.com/kienkien05/Rustlabel.git
cd Rustlabel
rustup update stable
cargo +stable build --locked --release --bins
```

On Windows, open `target\release\RustLabel.exe`. Run `./target/release/rustlabel` on Linux or macOS. Supply an image folder as the optional first argument.

```powershell
.\target\release\RustLabel.exe "C:\path\to\images"
```

```sh
./target/release/rustlabel "/path/to/images"
```

### Headless MCP

```sh
cargo +stable build --locked --release --no-default-features --bin rustlabel-mcp
```

Point the MCP client to the executable in `target/release`. Use `rustlabel-mcp.exe` on Windows or `rustlabel-mcp` on Linux and macOS. See [Connect an agent](#connect-an-agent) for arguments.

### GitHub Actions builds

[Build and test](https://github.com/kienkien05/Rustlabel/actions/workflows/build.yml) packages the desktop app and MCP server for Windows x64, Linux x64, and universal macOS (Apple Silicon and Intel). Open a successful run and download the archive for your system from **Artifacts**.

## Annotate

The interface defaults to Vietnamese. Use **Ngôn ngữ → English** to switch to English. The **Trợ giúp** (**Help**) tab has a quick-start guide and keyboard shortcuts in the selected language.

Use **Open > Folder** for a full folder, or **Open > Files** to select specific images on the same drive. You can also drop image files from your file manager onto RustLabel. A file selection only loads those images and their JSON sidecars. On mouse release, a rectangle stays in place while the label picker opens. Choose an existing label or type a new one and press Enter. Right-click a row in **Annotations** for **Change label…**, **Copy label**, or **Delete**. Select an annotation and press **Ctrl+E** (**Cmd+E** on macOS) to open the same label picker. Use **Delete** or **Backspace** on the canvas to remove the selected annotation.

The `Classes` tab lists every class found in the active selection. Folder mode also includes the root `labels.txt` (one per line). RustLabel caches the image list, the classes found so far, and the scan position next to the application session file. It checkpoints during scanning and saves the latest progress when closing or switching workspaces. Reopening the same folder or file selection resumes an unfinished scan at the saved position or uses the completed cache. Click **Refresh** in **Files** to pick up external additions, removals, or class changes. Refresh checks sidecar sizes and modification times and reads the JSON for new or changed annotations. The canvas, lists, and label picker share each class color. Compact tags attach to the outside of the top edge, aligned left, or use another outside edge when space is limited. Shapes keep a single class-colored outline without a black halo. Selected annotations keep their editing handles.

The left tool rail holds drawing modes. The default inspector on the right stacks **Annotations** and **Classes** above **Files**. Click an image row to open it. Files centers the active image when it changes, stopping at either end of the list. Use **A/D** or the arrow keys to move between images with Files focused. Drag across rows or use **Shift+Click** to select a range. Use **Ctrl+Click** (Cmd+Click on macOS) to toggle individual rows. A green checkmark marks images with a Labelme JSON sidecar. Click **Refresh** in **Files** to rescan the folder and update classes. In a workspace opened with **Open > Files**, Refresh checks the explicit file selection.

Right-click a row for **Cut**, **Copy**, **Paste**, **Copy Path**, **Reveal in File Manager**, **Rename**, and **Delete**. Paste uses the clicked image's folder. Right-click the file count or the empty list area to paste into the workspace root. Cut, copy, and paste work within the open RustLabel workspace. Copy, move, and rename keep each image paired with its JSON. Confirmed deletion moves the pair into a recovery folder under `.rustlabel/trash` within the image's parent folder. For batch class changes and JSON-only cleanup, use the context menu's **Annotations** submenu. **F2** renames a file when the Files list has focus. **Ctrl+Z** (**Cmd+Z** on macOS) undoes the latest file action in the open workspace, including restoring archived images and their JSON files.

Drag tabs onto another tab strip to group them, onto a docking arrow to split the work area, or into the canvas to float them. **Panels > Reset layout** restores the default. The **View** menu toggles annotations and label names. The checkbox beside a shape hides only its overlay.

On close, RustLabel caches the dock layout, open folder or explicit file selection, active image, file filter and selection, zoom and pan, and drawing preferences. Launch without a path to restore the last session from the app cache (`%APPDATA%\RustLabel\session.json` or a portable `session.json` beside the executable). The Windows GUI launches without a console window.

Filter the file list or navigate with A/D or the arrow keys. Press **1** for Select, **2** for Rectangle, **3** for Polygon, and **4** for Circle. Use **L** for Line, **G** for Polyline, and **O** for Point. Drag a circle from its center to its edge. Click polyline vertices and press Enter to finish. Close polygons with Enter or by clicking the first vertex. Click once on an annotation in Select or Rectangle mode to select it. Repeated clicks in an overlap cycle from the largest shape to the smallest. Switch to Select with **1** to move an annotation or drag its handles. Zoom at the cursor with Ctrl+wheel, pan with middle drag, and fit the image back to the canvas with **Ctrl+F** (**Cmd+F** on macOS).

Annotation changes autosave after label confirmation, a completed drag, deletion, undo, or redo. Drafts are not saved. Ctrl+S or Cmd+S still retries a failed save. Undo/redo apply to the current image, including already saved edits. View and per-shape visibility changes never write annotations.

## COCO and YOLO

- **Dataset > Import COCO**: choose a COCO JSON, the image root relative to its `file_name` entries, then an output parent on the same drive. Rectangles and single polygons are supported. Crowd/RLE masks, multipart polygons, and keypoints are not supported and stop the import with an error.
- **Dataset > Import YOLO**: choose a folder containing `images/` and `labels/`, or split folders such as `train/images/` and `train/labels/`. Detection boxes and segmentation polygons are supported; pose and oriented boxes are not supported. Class names come from dataset YAML, `classes.txt`, or `obj.names`. Without a class list, numeric IDs become `class_<id>`.
- **Dataset > Export**: choose COCO, YOLO Detection, or YOLO Segmentation. YOLO Detection converts polygons to enclosing boxes; YOLO Segmentation converts rectangle corners to polygons. Exports remap category IDs and do not preserve format-specific metadata. Train/val/test paths in YOLO YAML are included when they are identifiable from the folder layout; otherwise set the splits before training.

Import and export create a new directory under the chosen output parent, outside the source dataset. They hard-link images to avoid copying pixels; both locations must be on the same filesystem. RustLabel never modifies images, but changes to a hard-linked image made in other editors affect both locations. Editing and MCP share the resulting Labelme JSON workspace; original COCO/YOLO annotations are not overwritten.

Supports PNG, JPEG, BMP, and WebP. RustLabel draws rectangles, polygons, circles, lines, polylines, and points, and preserves unknown Labelme shape types and JSON fields when saving. These are also the supported shape types for MCP edits. COCO and YOLO export remain limited to rectangles and polygons. Labelme projects use a same-stem JSON sidecar beside each image; JSON-only projects with embedded images are not supported. Geometry uses original stored pixel dimensions; normalize EXIF rotation before labeling. Do not put two different image formats with the same filename stem in one folder.

The GUI refreshes externally updated annotations when clean. Save rejects a stale revision rather than overwriting an agent's edit. Use reload to discard a stale draft, or copy it elsewhere before resolving the conflict. Annotators coordinate with a file lock, atomic replacement, and previous-version JSON backups in the project's hidden `.rustlabel/history` directory. Third-party writers do not honor the lock; avoid saving the same image simultaneously in another application. Use a trusted local workspace, not a shared writeable folder controlled by another user.

## Connect an agent

Configure your MCP client to run the compiled `rustlabel-mcp` as a stdio process:

```json
{
  "mcpServers": {
    "rustlabel": {
      "command": "/absolute/path/to/rustlabel-mcp",
      "args": ["/absolute/path/to/images"]
    }
  }
}
```

On Windows, use an absolute path to `rustlabel-mcp.exe` and escape backslashes in JSON. Add `"--write"` after the workspace argument only for a workspace you want the agent to modify. Restart the MCP session to refresh its image inventory.

1. List images with `list_images`; optionally filter or paginate.
2. Call `inspect_image` for annotations, revision and original dimensions. Call `preview_image` when the agent needs to see the image; the MCP host decides how to handle returned image data.
3. With writes enabled, call `edit_annotations` with the inspected revision and `add`, `update` or `remove`. Coordinates are original-image pixels. Removal requires `confirm_remove: true`.

The server binds no port, uses no API key, makes no network requests, and cannot operate outside its configured workspace. There is no automatic image upload or inference. An agent can label from tool results; this version does not include an object detection model.

## Local checks

```sh
cargo test --locked --all-targets
cargo run --locked --release --example demo -- demo-workspace
```

The demo generates synthetic images in a new directory. CI builds and tests the two executables on Windows, Linux, and macOS.
