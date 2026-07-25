use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    id: String,
    name: String,
    version: String,
    description: Option<String>,
    extension: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageRecord {
    manifest: PackageManifest,
    module_path: PathBuf,
    enabled: bool,
}

#[derive(Default)]
pub struct PackageManager;

impl PackageManager {
    pub fn new() -> Self {
        Self
    }

    pub fn list_packages(&self, project_root: &Path) -> Vec<PackageRecord> {
        let Ok(Some(extensions_dir)) = resolve_extensions_dir(project_root, false) else {
            return Vec::new();
        };
        let Ok(entries) = fs::read_dir(extensions_dir) else {
            return Vec::new();
        };

        entries
            .flatten()
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| {
                let mut record = self.package_record(&entry.path()).ok()?;
                record.module_path = project_root
                    .join(".threadlane/extensions")
                    .join(entry.file_name())
                    .join("extension.wasm");
                Some(record)
            })
            .collect()
    }

    pub fn install_from_local(
        &self,
        source: &Path,
        project_root: &Path,
    ) -> Result<PackageRecord, String> {
        let source = source
            .canonicalize()
            .map_err(|e| format!("Failed to resolve package source: {e}"))?;
        if !source.is_dir() {
            return Err("Package source must be a directory".into());
        }

        let manifest_file = source.join("threadlane-package.json");
        let contents = fs::read_to_string(&manifest_file)
            .map_err(|e| format!("Failed to read package manifest: {e}"))?;
        let mut manifest: PackageManifest = serde_json::from_str(&contents)
            .map_err(|e| format!("Invalid threadlane-package.json manifest: {e}"))?;
        validate_package_id(&manifest.id)?;

        let module = source.join(&manifest.extension);
        if manifest.extension.is_absolute() {
            return Err("Package extension must be relative to the source directory".into());
        }
        if manifest.extension.extension() != Some(OsStr::new("wasm")) {
            return Err("Package extension must be a .wasm file".into());
        }
        let module = module
            .canonicalize()
            .map_err(|e| format!("Failed to resolve package extension: {e}"))?;
        if !module.starts_with(&source) {
            return Err("Package extension must remain below the source directory".into());
        }
        if !module.is_file() {
            return Err("Package extension must be a file".into());
        }

        let extensions_dir = resolve_extensions_dir(project_root, true)?
            .ok_or_else(|| "Failed to create extensions directory".to_string())?;
        let target = extensions_dir.join(&manifest.id);
        let installed_module = project_root
            .join(".threadlane/extensions")
            .join(&manifest.id)
            .join("extension.wasm");
        let target_exists = match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("Package destination must not be a symbolic link".into())
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err("Package destination must be a directory".into())
            }
            Ok(_) => true,
            Err(error) if error.kind() == ErrorKind::NotFound => false,
            Err(error) => {
                return Err(format!("Failed to inspect package destination: {error}"))
            }
        };
        let staged = create_staging_dir(&extensions_dir, &manifest.id)?;
        let result = (|| {
            manifest.extension = PathBuf::from("extension.wasm");
            fs::write(
                staged.join("threadlane-package.json"),
                serde_json::to_vec_pretty(&manifest)
                    .map_err(|e| format!("Failed to serialize package manifest: {e}"))?,
            )
            .map_err(|e| format!("Failed to stage package manifest: {e}"))?;
            fs::copy(&module, staged.join("extension.wasm"))
                .map_err(|e| format!("Failed to stage package extension: {e}"))?;

            let mut record = self.package_record(&staged)?;
            if target_exists {
                let backup = available_path(&extensions_dir, &manifest.id, "backup");
                fs::rename(&target, &backup)
                    .map_err(|e| format!("Failed to back up existing package: {e}"))?;
                if let Err(error) = fs::rename(&staged, &target) {
                    let restore = fs::rename(&backup, &target);
                    return Err(match restore {
                        Ok(()) => format!("Failed to install package replacement: {error}"),
                        Err(restore_error) => format!(
                            "Failed to install package replacement: {error}; failed to restore previous package: {restore_error}"
                        ),
                    });
                }
                let _ = fs::remove_dir_all(&backup);
            } else {
                fs::rename(&staged, &target)
                    .map_err(|e| format!("Failed to install package: {e}"))?;
            }

            record.module_path = installed_module;
            Ok(record)
        })();
        if staged.exists() {
            let _ = fs::remove_dir_all(&staged);
        }
        result
    }

    pub fn remove_package(&self, package_id: &str, project_root: &Path) -> Result<(), String> {
        validate_package_id(package_id)?;
        let Some(extensions_dir) = resolve_extensions_dir(project_root, false)? else {
            return Ok(());
        };
        let target = extensions_dir.join(package_id);
        let metadata = match fs::symlink_metadata(&target) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "Failed to inspect package '{package_id}': {error}"
                ))
            }
        };
        if metadata.file_type().is_symlink() {
            return Err("Package destination must not be a symbolic link".into());
        }
        if !metadata.is_dir() {
            return Err("Package destination must be a directory".into());
        }
        let target = target
            .canonicalize()
            .map_err(|e| format!("Failed to resolve package '{package_id}': {e}"))?;
        if target.parent() != Some(extensions_dir.as_path()) {
            return Err("Package destination must remain below the project extension root".into());
        }
        fs::remove_dir_all(&target)
            .map_err(|e| format!("Failed to remove package '{package_id}': {e}"))?;
        Ok(())
    }

    fn package_record(&self, package_dir: &Path) -> Result<PackageRecord, String> {
        let contents = fs::read_to_string(package_dir.join("threadlane-package.json"))
            .map_err(|e| format!("Failed to read package manifest: {e}"))?;
        let manifest: PackageManifest = serde_json::from_str(&contents)
            .map_err(|e| format!("Invalid threadlane-package.json manifest: {e}"))?;
        validate_package_id(&manifest.id)?;
        let module_path = package_dir.join("extension.wasm");
        if manifest.extension != Path::new("extension.wasm") || !module_path.is_file() {
            return Err("Package extension is missing or invalid".into());
        }
        Ok(PackageRecord {
            manifest,
            module_path,
            enabled: true,
        })
    }
}

impl PackageRecord {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    pub fn module_path(&self) -> &Path {
        &self.module_path
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

fn resolve_extensions_dir(project_root: &Path, create: bool) -> Result<Option<PathBuf>, String> {
    let project_root = project_root
        .canonicalize()
        .map_err(|e| format!("Failed to resolve project root: {e}"))?;
    if !project_root.is_dir() {
        return Err("Project root must be a directory".into());
    }

    let threadlane_dir = project_root.join(".threadlane");
    let extensions_dir = threadlane_dir.join("extensions");
    for directory in [&threadlane_dir, &extensions_dir] {
        match fs::symlink_metadata(directory) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "Extension destination component '{}' must not be a symbolic link",
                    directory.display()
                ))
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!(
                    "Extension destination component '{}' must be a directory",
                    directory.display()
                ))
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound && create => {
                fs::create_dir(directory).map_err(|e| {
                    format!(
                        "Failed to create extension destination '{}': {e}",
                        directory.display()
                    )
                })?;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "Failed to inspect extension destination '{}': {error}",
                    directory.display()
                ))
            }
        }
    }

    let resolved = extensions_dir
        .canonicalize()
        .map_err(|e| format!("Failed to resolve project extension root: {e}"))?;
    if !resolved.starts_with(&project_root) || resolved != extensions_dir {
        return Err("Project extension root must remain within the project".into());
    }
    Ok(Some(resolved))
}

fn validate_package_id(id: &str) -> Result<(), String> {
    let mut chars = id.bytes();
    let Some(first) = chars.next() else {
        return Err("Package ID must not be empty".into());
    };
    if !first.is_ascii_alphanumeric()
        || !chars.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("Package ID must begin with an ASCII letter or digit and contain only ASCII letters, digits, '-', '_', or '.'".into());
    }
    Ok(())
}

fn create_staging_dir(parent: &Path, id: &str) -> Result<PathBuf, String> {
    for suffix in 0..1000 {
        let path = parent.join(format!(".{id}.staged-{}-{suffix}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Failed to create package staging directory: {error}"
                ))
            }
        }
    }
    Err("Failed to allocate package staging directory".into())
}

fn available_path(parent: &Path, id: &str, kind: &str) -> PathBuf {
    for suffix in 0..1000 {
        let path = parent.join(format!(".{id}.{kind}-{}-{suffix}", std::process::id()));
        if !path.exists() {
            return path;
        }
    }
    parent.join(format!(".{id}.{kind}-{}", std::process::id()))
}
