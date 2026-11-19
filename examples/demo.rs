use anyhow::{Context, Result, ensure};
use image::{Rgb, RgbImage};
use rustlabel::{Workspace, shape, shapes_mut};
use std::{fs, path::PathBuf};

fn main() -> Result<()> {
    let path = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("Expected a new demo folder")?,
    );
    fs::create_dir(&path).context("Demo folder must not already exist")?;
    let size = [960, 540];
    for (name, offset) in [("frame-001.png", 0), ("frame-002.png", 140)] {
        let offset = offset as i32;
        let image = RgbImage::from_fn(size[0], size[1], |x, y| {
            let (x, y) = (x as i32, y as i32);
            let rgb = match (x, y) {
                (x, y) if (x - 220).pow(2) + (y - 196).pow(2) < 23 * 23 => [195, 81, 54],
                (212..=228, 210..=326) => [126, 130, 130],
                (x, y) if (350 + offset..530 + offset).contains(&x) && (320..396).contains(&y) => {
                    [154, 91, 68]
                }
                (x, y) if (390 + offset..492 + offset).contains(&x) && (292..324).contains(&y) => {
                    [178, 123, 99]
                }
                (x, y)
                    if (x - 387 - offset).pow(2) + (y - 394).pow(2) < 15 * 15
                        || (x - 497 - offset).pow(2) + (y - 394).pow(2) < 15 * 15 =>
                {
                    [27, 28, 29]
                }
                (x, y) if y > 305 && (x - 480).abs() < (y - 266) * 3 => {
                    if y > 419
                        && x.rem_euclid(200) < 115
                        && ((y - 439).abs() < 5 || (y - 524).abs() < 7)
                    {
                        [222, 223, 203]
                    } else {
                        [74, 80, 83]
                    }
                }
                (_, y) if y > 282 => [104, 135, 99],
                (x, y) if y > 206 && (x % 197) < 111 => [143, 156, 158],
                _ => [187, 214, 223],
            };
            Rgb(rgb)
        });
        image.save(path.join(name))?;
    }
    let workspace = Workspace::open(&path)?;
    let mut snapshot = workspace.inspect("frame-001.png")?;
    ensure!(snapshot.revision == "missing");
    let shapes = shapes_mut(&mut snapshot.doc)?;
    shapes.push(shape(
        "car",
        "rectangle",
        &[[350., 291.], [531., 411.]],
        size,
    )?);
    shapes.push(shape(
        "traffic_sign",
        "rectangle",
        &[[196., 171.], [245., 220.]],
        size,
    )?);
    workspace.save("frame-001.png", &snapshot.revision, &snapshot.doc)?;
    Ok(())
}
