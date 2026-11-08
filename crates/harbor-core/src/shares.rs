use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use walkdir::WalkDir;

use crate::error::{HarborError, Result};
use crate::sync::ManifestEntry;
use crate::types::Entry;
use crate::util::expand_tilde;

/// M2 单次清单的硬上限。超过后建议拆分共享目录（后续可改为流式游标）。
pub const MAX_MANIFEST_FILES: usize = 500_000;

#[derive(Debug, Clone, Default)]
pub struct ManifestScan {
    pub entries: Vec<ManifestEntry>,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ShareStats {
    pub files: u64,
    pub bytes: u64,
}

/// 扫描目录，统计文件数与总字节数。跳过符号链接，不跟随目录外内容。
pub fn scan_stats(root: &Path) -> ShareStats {
    let mut stats = ShareStats::default();
    for entry in WalkDir::new(root).follow_links(false).into_iter().flatten() {
        if entry.file_type().is_symlink() {
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            stats.files += 1;
            stats.bytes += meta.len();
        }
    }
    stats
}

/// 内置忽略规则：版本库/依赖目录、系统垃圾文件、Harbor 自己的临时与隔离目录。
pub fn is_ignored_name(name: &str) -> bool {
    matches!(
        name,
        ".harbor"
            | ".harbor-trash"
            | ".git"
            | "node_modules"
            | ".DS_Store"
            | "Thumbs.db"
            | "ehthumbs.db"
            | "desktop.ini"
            | "$RECYCLE.BIN"
    ) || name.ends_with(".tmp")
        || name.starts_with("~$")
}

/// 扫描共享目录，生成按路径排序的清单快照。目录会保留（包括空目录），
/// 符号链接与特殊文件一律跳过。
pub fn scan_manifest(root: &Path) -> Result<ManifestScan> {
    let root = root
        .canonicalize()
        .map_err(|_| HarborError::NotFound(root.display().to_string()))?;
    let mut scan = ManifestScan::default();

    for item in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            entry.depth() == 0 || !is_ignored_name(&name)
        })
        .flatten()
    {
        if item.depth() == 0 {
            continue;
        }
        if item.file_type().is_symlink() {
            continue;
        }
        let Ok(relative) = item.path().strip_prefix(&root) else {
            continue;
        };
        let rel = relative
            .to_string_lossy()
            .replace('\\', "/")
            .trim_start_matches('/')
            .to_string();
        if rel.is_empty() {
            continue;
        }
        let Ok(meta) = item.metadata() else {
            continue;
        };
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as u64);
        let mode = file_mode(&meta);

        if meta.is_dir() {
            scan.entries.push(ManifestEntry {
                path: rel,
                is_dir: true,
                size: 0,
                mtime_ms,
                mode,
            });
        } else if meta.is_file() {
            scan.files += 1;
            scan.bytes = scan.bytes.saturating_add(meta.len());
            if scan.files as usize > MAX_MANIFEST_FILES {
                return Err(HarborError::Other(format!(
                    "共享目录文件数量超过 {}，请拆分后再共享",
                    MAX_MANIFEST_FILES
                )));
            }
            scan.entries.push(ManifestEntry {
                path: rel,
                is_dir: false,
                size: meta.len(),
                mtime_ms,
                mode,
            });
        }
    }

    scan.entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(scan)
}

#[cfg(unix)]
fn file_mode(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn file_mode(_meta: &fs::Metadata) -> u32 {
    0
}

/// 校验并规范化用户选择的共享目录。
pub fn validate_share_dir(input: &str) -> Result<PathBuf> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(HarborError::InvalidPath);
    }
    let path = expand_tilde(raw);
    let canonical = path
        .canonicalize()
        .map_err(|_| HarborError::NotFound(raw.to_string()))?;
    if !canonical.is_dir() {
        return Err(HarborError::NotFound(raw.to_string()));
    }
    if canonical.parent().is_none() {
        return Err(HarborError::InvalidPath);
    }
    Ok(canonical)
}

pub fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "共享文件夹".to_string())
}

/// 相对路径拆解：拒绝绝对路径、`..`、NUL 与盘符。
fn safe_components(rel: &str) -> Result<Vec<String>> {
    if rel.starts_with('/') || rel.starts_with('\\') || rel.contains('\0') {
        return Err(HarborError::InvalidPath);
    }
    let mut out = Vec::new();
    for part in rel.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(HarborError::InvalidPath);
        }
        if part.len() > 255 {
            return Err(HarborError::InvalidPath);
        }
        out.push(part.to_string());
    }
    Ok(out)
}

/// 解析共享根下的路径，并确保 canonicalize 之后仍在根内。
pub fn resolve_in_share(root: &Path, rel: &str) -> Result<PathBuf> {
    let parts = safe_components(rel)?;
    let root = root
        .canonicalize()
        .map_err(|_| HarborError::NotFound(root.display().to_string()))?;
    let mut joined = root.clone();
    for part in &parts {
        joined.push(part);
    }
    let canonical = joined
        .canonicalize()
        .map_err(|_| HarborError::NotFound(rel.to_string()))?;
    if !canonical.starts_with(&root) {
        return Err(HarborError::InvalidPath);
    }
    Ok(canonical)
}

/// 解析共享根下的普通文件，逐级拒绝符号链接。
pub fn resolve_file_in_share(root: &Path, rel: &str) -> Result<PathBuf> {
    let parts = safe_components(rel)?;
    let root = root
        .canonicalize()
        .map_err(|_| HarborError::NotFound(root.display().to_string()))?;
    let mut current = root.clone();
    for part in &parts {
        current.push(part);
        let meta =
            fs::symlink_metadata(&current).map_err(|_| HarborError::NotFound(rel.to_string()))?;
        if meta.file_type().is_symlink() {
            return Err(HarborError::InvalidPath);
        }
    }
    let canonical = current
        .canonicalize()
        .map_err(|_| HarborError::NotFound(rel.to_string()))?;
    if !canonical.starts_with(&root) || !canonical.is_file() {
        return Err(HarborError::InvalidPath);
    }
    Ok(canonical)
}

pub fn list_dir(root: &Path, rel: &str) -> Result<Vec<Entry>> {
    let dir = resolve_in_share(root, rel)?;
    if !dir.is_dir() {
        return Err(HarborError::InvalidPath);
    }

    // 防御性检查：canonical 路径的每一级都不得是符号链接。
    if dir != root.canonicalize().unwrap_or_else(|_| root.to_path_buf()) {
        let mut cursor = root.to_path_buf();
        for part in safe_components(rel)? {
            cursor.push(part);
            let meta = fs::symlink_metadata(&cursor)?;
            if meta.file_type().is_symlink() {
                return Err(HarborError::InvalidPath);
            }
        }
    }

    let base_rel = safe_components(rel)?.join("/");
    let mut entries = Vec::new();
    let read =
        fs::read_dir(&dir).map_err(|e| HarborError::Storage(format!("读取目录失败：{e}")))?;

    for item in read.flatten().take(5000) {
        let name = item.file_name().to_string_lossy().to_string();
        let meta = match item.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if meta.is_symlink() {
            continue;
        }
        let md = match item.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_dir = md.is_dir();
        let modified_ms = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64);
        let path = if base_rel.is_empty() {
            name.clone()
        } else {
            format!("{base_rel}/{name}")
        };
        entries.push(Entry {
            name,
            path,
            is_dir,
            size: if md.is_file() { md.len() } else { 0 },
            modified_ms,
        });
    }

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Windows 文件名清洗（M1 仅用于未来写入路径，当前浏览不需要）。
pub fn sanitize_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    while out.ends_with(' ') || out.ends_with('.') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("unnamed");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("harbor-share-{}", rand::random::<u64>()));
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), b"hello").unwrap();
        fs::write(dir.join("sub/b.txt"), b"world!").unwrap();
        dir
    }

    #[test]
    fn lists_and_scans() {
        let dir = temp_dir();
        let entries = list_dir(&dir, "").unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_dir);
        let sub = list_dir(&dir, "sub").unwrap();
        assert_eq!(sub[0].name, "b.txt");
        let stats = scan_stats(&dir);
        assert_eq!(stats.files, 2);
        assert_eq!(stats.bytes, 11);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_traversal() {
        let dir = temp_dir();
        for bad in ["..", "../etc", "sub/../../", "/etc", "a/../../b"] {
            assert!(matches!(
                resolve_in_share(&dir, bad),
                Err(HarborError::InvalidPath)
            ));
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn manifest_includes_dirs_and_skips_junk() {
        let dir = temp_dir();
        fs::create_dir_all(dir.join("empty")).unwrap();
        fs::write(dir.join(".DS_Store"), b"junk").unwrap();
        fs::write(dir.join("a.tmp"), b"junk").unwrap();
        fs::create_dir_all(dir.join(".harbor/parts")).unwrap();
        fs::write(dir.join(".harbor/parts/x.part"), b"junk").unwrap();

        let scan = scan_manifest(&dir).unwrap();
        let paths: Vec<&str> = scan
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert_eq!(paths, vec!["a.txt", "empty", "sub", "sub/b.txt"]);
        assert_eq!(scan.files, 2);
        assert_eq!(scan.bytes, 11);
        assert!(scan
            .entries
            .iter()
            .any(|entry| entry.path == "empty" && entry.is_dir));

        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn skips_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir();
        let outside = std::env::temp_dir().join("harbor-outside.txt");
        fs::write(&outside, b"secret").unwrap();
        let link = dir.join("link.txt");
        let _ = fs::remove_file(&link);
        symlink(&outside, &link).unwrap();
        let entries = list_dir(&dir, "").unwrap();
        assert!(!entries.iter().any(|e| e.name == "link.txt"));
        assert!(matches!(
            resolve_file_in_share(&dir, "link.txt"),
            Err(HarborError::InvalidPath)
        ));
        let _ = fs::remove_file(&link);
        let _ = fs::remove_file(&outside);
        fs::remove_dir_all(&dir).ok();
    }
}
