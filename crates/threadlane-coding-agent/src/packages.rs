use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackageScope {
    Global,
    Project,
}

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
    scope: PackageScope,
    root_dir: PathBuf,
    module_path: PathBuf,
    enabled: bool,
}

pub struct PackageManager;

impl PackageManager {
    pub fn new() -> Self {
        Self
    }

    pub fn list_packages(&self, project_root: &Path) -> Vec<PackageRecord> {
        let extensions_dir = extensions_dir(project_root);
        let Ok(entries) = fs::read_dir(extensions_dir) else {
            return Vec::new();
        };

        entries
            .flatten()
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .filter_map(|entry| self.package_record(&entry.path()).ok())
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

        let extensions_dir = extensions_dir(project_root);
        fs::create_dir_all(&extensions_dir)
            .map_err(|e| format!("Failed to create extensions directory: {e}"))?;
        let target = extensions_dir.join(&manifest.id);
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

            if target.exists() {
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

            self.package_record(&target)
        })();
        if staged.exists() {
            let _ = fs::remove_dir_all(&staged);
        }
        result
    }

    pub fn remove_package(&self, package_id: &str, project_root: &Path) -> Result<(), String> {
        validate_package_id(package_id)?;
        let target = extensions_dir(project_root).join(package_id);
        if target.exists() {
            fs::remove_dir_all(&target)
                .map_err(|e| format!("Failed to remove package '{package_id}': {e}"))?;
        }
        Ok(())
    }

    fn package_record(&self, root_dir: &Path) -> Result<PackageRecord, String> {
        let contents = fs::read_to_string(root_dir.join("threadlane-package.json"))
            .map_err(|e| format!("Failed to read package manifest: {e}"))?;
        let manifest: PackageManifest = serde_json::from_str(&contents)
            .map_err(|e| format!("Invalid threadlane-package.json manifest: {e}"))?;
        validate_package_id(&manifest.id)?;
        let module_path = root_dir.join("extension.wasm");
        if manifest.extension != Path::new("extension.wasm") || !module_path.is_file() {
            return Err("Package extension is missing or invalid".into());
        }
        Ok(PackageRecord {
            manifest,
            scope: PackageScope::Project,
            root_dir: root_dir.to_path_buf(),
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

    pub fn scope(&self) -> PackageScope {
        self.scope
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

fn extensions_dir(project_root: &Path) -> PathBuf {
    project_root.join(".threadlane/extensions")
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
