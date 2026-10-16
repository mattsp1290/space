//! Per-user cache location and self-exclusion. Display strings never become action paths.
use space_core::{NativeName, ScanPolicy};
use std::path::{Component, Path, PathBuf};

pub fn cache_directory() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or("LOCALAPPDATA is unavailable; cannot choose a private cache location.")?;
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is unavailable; cannot choose a private cache location.")?
        .join("Library/Application Support");
    #[cfg(target_os = "linux")]
    let base = match std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is unavailable; cannot choose a private cache location.")?
            .join(".local/share"),
    };
    if !base.is_absolute() {
        return Err("Application-data location must be an absolute path.".into());
    }
    Ok(base.join("Space"))
}

pub fn scan_policy(root: &Path, cache: &Path) -> Result<ScanPolicy, String> {
    let mut policy = ScanPolicy::default();
    if let Ok(relative) = cache.strip_prefix(root) {
        if relative.as_os_str().is_empty() {
            return Err("The application's own cache folder cannot be scanned.".into());
        }
        let components = relative
            .components()
            .map(|component| match component {
                Component::Normal(value) => {
                    NativeName::from_os(value).map_err(|error| error.to_string())
                }
                _ => Err("Cache exclusion contains an unexpected path component.".into()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        policy.exclusions.push(components);
    }
    policy.normalize();
    policy.validate().map_err(|error| error.to_string())?;
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_subtree_is_excluded() {
        let root = std::env::temp_dir();
        let cache = root.join("space-fixture-cache");
        let policy = scan_policy(&root, &cache).unwrap();
        assert_eq!(policy.exclusions.len(), 1);
        assert_eq!(policy.exclusions[0][0].display(), "space-fixture-cache");
    }
    #[test]
    fn scanning_cache_itself_is_rejected() {
        let cache = std::env::temp_dir().join("space-fixture-cache");
        assert!(scan_policy(&cache, &cache).is_err());
    }
}
