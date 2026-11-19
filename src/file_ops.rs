use super::*;

impl Workspace {
    /// Copy, move, or rename an image and its Labelme sidecar together.
    pub fn transfer_image(
        &mut self,
        name: &str,
        folder: &Path,
        new_name: Option<&str>,
        move_source: bool,
    ) -> Result<String> {
        let image = self.image_path(name)?;
        let sidecar = self.sidecar(&image)?;
        let folder = folder.canonicalize()?;
        ensure!(
            folder.starts_with(&self.root)
                && folder.is_dir()
                && folder.strip_prefix(&self.root)?.components().all(|part| {
                    matches!(part, Component::Normal(name) if !name.to_string_lossy().starts_with('.'))
                }),
            "Choose a folder inside the workspace"
        );
        let original = image
            .file_name()
            .and_then(|n| n.to_str())
            .context("Invalid image name")?;
        let filename = new_name.unwrap_or(original);
        ensure!(
            !filename.starts_with('.')
                && !filename.ends_with([' ', '.'])
                && !filename
                    .chars()
                    .any(|c| c.is_control() || r#"/\:<>|?*""#.contains(c))
                && Path::new(filename).extension().is_some_and(|ext| {
                    ext.eq_ignore_ascii_case(image.extension().unwrap_or_default())
                }),
            "Enter a valid filename with the original extension"
        );
        if move_source && folder.join(filename) == image {
            return Ok(name.to_owned());
        }

        let mut reserved = HashSet::new();
        for entry in fs::read_dir(&folder)? {
            let path = entry?.path();
            if move_source && (path == image || path == sidecar) {
                continue;
            }
            if is_image(&path)
                || path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
            {
                reserved.insert(sidecar_key(&path));
            }
        }
        let mut target = folder.join(filename);
        if !move_source && new_name.is_none() && reserved.contains(&sidecar_key(&target)) {
            let stem = image.file_stem().unwrap_or_default().to_string_lossy();
            let ext = image.extension().unwrap_or_default().to_string_lossy();
            target = (1..)
                .map(|i| folder.join(format!("{stem} ({i}).{ext}")))
                .find(|path| !reserved.contains(&sidecar_key(path)))
                .context("Could not find a free filename")?;
        }
        ensure!(
            !reserved.contains(&sidecar_key(&target)),
            "An image or annotation already uses that name"
        );
        let relative = target
            .strip_prefix(&self.root)?
            .to_str()
            .context("Invalid destination name")?
            .replace(std::path::MAIN_SEPARATOR, "/");
        ensure!(!relative.contains(['\\', ':']), "Invalid destination path");
        let target_json = target.with_extension("json");
        let mut lock_paths = [&sidecar, &target_json];
        lock_paths.sort_by_key(|path| sidecar_key(path));
        let _first_lock = lock_sidecar(lock_paths[0])?;
        let _second_lock = (sidecar_key(&sidecar) != sidecar_key(&target_json))
            .then(|| lock_sidecar(lock_paths[1]))
            .transpose()?;
        self.image_path(name)?;
        let same_key = move_source && sidecar_key(&image) == sidecar_key(&target);
        if !same_key {
            ensure!(
                fs::symlink_metadata(&target)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                    && annotation_stamp(&target_json)?.is_none(),
                "An image or annotation already uses that name"
            );
        }

        let mut originals = vec![image.as_path()];
        let json_temp = if annotation_stamp(&sidecar)?.is_some() {
            let mut doc = self.inspect(name)?.doc;
            doc["imagePath"] = json!(target.file_name().and_then(|n| n.to_str()));
            let mut temp = tempfile::NamedTempFile::new_in(&folder)?;
            let mut data = serde_json::to_vec_pretty(&doc)?;
            data.push(b'\n');
            ensure!(
                data.len() as u64 <= MAX_JSON,
                "Annotation exceeds the size limit"
            );
            temp.write_all(&data)?;
            temp.as_file().sync_all()?;
            originals.push(&sidecar);
            Some(temp)
        } else {
            None
        };
        let archived = move_source.then(|| archive_files(&originals)).transpose()?;
        let result = (|| -> Result<()> {
            let archived_image = archived.as_ref().map(|dir| dir.path().join(original));
            if archived_image
                .as_ref()
                .is_none_or(|path| fs::hard_link(path, &target).is_err())
            {
                publish_copy(archived_image.as_deref().unwrap_or(&image), &target)?;
            }
            if let Some(temp) = json_temp
                && let Err(error) = temp.persist_noclobber(&target_json)
            {
                fs::remove_file(&target).context("Could not roll back the new image")?;
                return Err(error.into());
            }
            Ok(())
        })();
        if let Err(error) = result {
            if let Some(archived) = archived {
                restore_files(archived, &originals)?;
            }
            return Err(error);
        }
        self.update_inventory(move_source.then_some(name), Some(relative.clone()));
        Ok(relative)
    }

    pub fn archive_image(&mut self, name: &str) -> Result<PathBuf> {
        let image = self.image_path(name)?;
        let sidecar = self.sidecar(&image)?;
        let _lock = lock_sidecar(&sidecar)?;
        let mut files = vec![image.as_path()];
        if annotation_stamp(&sidecar)?.is_some() {
            files.push(&sidecar);
        }
        let archived = archive_files(&files)?.keep();
        self.update_inventory(Some(name), None);
        Ok(archived)
    }

    pub fn restore_archive(
        &mut self,
        name: &str,
        archive: &Path,
        annotations_only: bool,
    ) -> Result<()> {
        let relative = Path::new(name);
        ensure!(
            !name.contains(['\\', ':'])
                && is_image(relative)
                && relative.components().all(|part| {
                    matches!(part, Component::Normal(name) if !name.to_string_lossy().starts_with('.'))
                }),
            "Invalid image path"
        );
        let image = self.root.join(relative);
        let folder = image.parent().context("Missing image folder")?;
        ensure!(folder.canonicalize()? == folder, "Image folder changed");
        ensure!(
            archive.parent() == Some(folder.join(PRIVATE).join("trash").as_path())
                && archive.canonicalize()? == archive,
            "Invalid recovery folder"
        );
        let sidecar = image.with_extension("json");
        let _lock = lock_sidecar(&sidecar)?;
        let mut targets = Vec::new();
        if annotations_only {
            self.sidecar_path(name)?;
        } else {
            for entry in fs::read_dir(folder)? {
                let path = entry?.path();
                ensure!(
                    !is_image(&path) || sidecar_key(&path) != sidecar_key(&image),
                    "An image already uses that name"
                );
            }
            self.images()?;
            targets.push(image.as_path());
        }
        ensure!(
            annotation_stamp(&sidecar)?.is_none(),
            "An annotation already uses that name"
        );
        let archived_json = archive.join(sidecar.file_name().context("Missing annotation name")?);
        if annotations_only || annotation_stamp(&archived_json)?.is_some() {
            targets.push(&sidecar);
        }
        restore_archived_files(archive, &targets)?;
        if !annotations_only {
            self.update_inventory(None, Some(name.to_owned()));
        }
        Ok(())
    }

    fn update_inventory(&mut self, removed: Option<&str>, added: Option<String>) {
        let index = Arc::make_mut(&mut self.index)
            .get_mut()
            .expect("Index initialized");
        if let Some(removed) = removed {
            index.images.retain(|name| name != removed);
        }
        index.images.extend(added);
        index.images.sort_unstable();
        let mut seen = HashSet::new();
        index.collisions.clear();
        for name in &index.images {
            let key = sidecar_key(&self.root.join(name));
            if !seen.insert(key.clone()) {
                index.collisions.insert(key);
            }
        }
    }
}

fn publish_copy(source: &Path, target: &Path) -> Result<()> {
    let folder = target.parent().context("Missing destination folder")?;
    let mut temp = tempfile::NamedTempFile::new_in(folder)?;
    std::io::copy(&mut fs::File::open(source)?, &mut temp)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(target)?;
    Ok(())
}

fn restore_archived_files(archive: &Path, targets: &[&Path]) -> Result<()> {
    let mut restored = Vec::new();
    for target in targets {
        let source = archive.join(target.file_name().context("Missing filename")?);
        let result = (|| -> Result<()> {
            ensure!(
                annotation_stamp(&source)?.is_some(),
                "Recovery file is missing"
            );
            if fs::hard_link(&source, target).is_err() {
                publish_copy(&source, target)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            for (_, target) in restored {
                fs::remove_file(target).context("Could not roll back restored file")?;
            }
            return Err(error);
        }
        restored.push((source, *target));
    }
    for (source, _) in restored {
        let _ = fs::remove_file(source);
    }
    let _ = fs::remove_dir(archive);
    Ok(())
}

pub(super) fn archive_files(files: &[&Path]) -> Result<tempfile::TempDir> {
    let folder = files[0].parent().context("Missing file folder")?;
    let trash = checked_dir(&checked_dir(folder, PRIVATE)?, "trash")?;
    let archive = tempfile::tempdir_in(trash)?;
    for (index, path) in files.iter().enumerate() {
        let target = archive
            .path()
            .join(path.file_name().context("Missing filename")?);
        if let Err(error) = fs::rename(path, target) {
            restore_files(archive, &files[..index])?;
            return Err(error).with_context(|| format!("Could not move {}", path.display()));
        }
    }
    Ok(archive)
}

fn restore_files(archive: tempfile::TempDir, files: &[&Path]) -> Result<()> {
    let mut failure = None;
    for path in files.iter().rev() {
        let saved = archive
            .path()
            .join(path.file_name().context("Missing filename")?);
        if let Err(error) = fs::hard_link(saved, path) {
            failure = Some(error);
        }
    }
    if let Some(error) = failure {
        let archive = archive.keep();
        return Err(error)
            .with_context(|| format!("Recover original files from {}", archive.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_operations_keep_labelme_pairs_and_refuse_collisions() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let image = root.join("one.png");
        image::RgbImage::new(12, 8).save(&image)?;
        image::RgbImage::new(12, 8).save(root.join("occupied.jpg"))?;
        let mut workspace = Workspace::open_files(std::slice::from_ref(&image))?;
        let mut snapshot = workspace.inspect("one.png")?;
        snapshot.doc["extra"] = json!({"keep": true});
        snapshot.doc["shapes"] = json!([shape("cat", "point", &[[1., 2.]], [12, 8])?]);
        workspace.save("one.png", &snapshot.revision, &snapshot.doc)?;
        let stale = workspace.clone();
        let old_revision = stale.inspect("one.png")?.revision;

        assert!(
            workspace
                .transfer_image("one.png", root, Some("occupied.png"), true)
                .is_err()
        );
        assert!(
            workspace
                .transfer_image("one.png", root, Some("../outside.png"), true)
                .is_err()
        );
        let copy = workspace.transfer_image("one.png", root, None, false)?;
        assert_eq!(copy, "one (1).png");
        assert_eq!(fs::read(root.join(&copy))?, fs::read(&image)?);
        let copied = workspace.inspect(&copy)?;
        assert_eq!(copied.doc["imagePath"], copy);
        assert_eq!(copied.doc["extra"], snapshot.doc["extra"]);
        assert_eq!(copied.doc["shapes"], snapshot.doc["shapes"]);

        let renamed = workspace.transfer_image("one.png", root, Some("ONE.png"), true)?;
        assert_eq!(renamed, "ONE.png");
        assert_eq!(workspace.inspect(&renamed)?.doc["imagePath"], renamed);
        let subfolder = root.join("other");
        fs::create_dir(&subfolder)?;
        let moved = workspace.transfer_image(&renamed, &subfolder, None, true)?;
        assert_eq!(moved, "other/ONE.png");
        assert!(!root.join("one.json").exists());
        assert!(stale.save("one.png", &old_revision, &snapshot.doc).is_err());

        let archived = workspace.archive_image(&moved)?;
        assert!(archived.join("ONE.png").is_file());
        assert!(archived.join("ONE.json").is_file());
        assert!(!root.join(&moved).exists());
        assert_eq!(workspace.images()?, std::slice::from_ref(&copy));
        fs::write(subfolder.join("ONE.jpg"), [])?;
        assert!(workspace.restore_archive(&moved, &archived, false).is_err());
        assert!(archived.join("ONE.json").is_file());
        fs::remove_file(subfolder.join("ONE.jpg"))?;
        workspace.restore_archive(&moved, &archived, false)?;
        assert_eq!(
            workspace.inspect(&moved)?.doc["extra"],
            snapshot.doc["extra"]
        );
        assert_eq!(workspace.images()?, &[copy, moved]);
        assert!(root.join("occupied.jpg").is_file());
        Ok(())
    }
}
