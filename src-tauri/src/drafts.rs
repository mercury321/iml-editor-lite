use std::{fs, io::{ErrorKind, Read}, path::{Path, PathBuf}, sync::Mutex};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

#[derive(Default)]
pub struct DraftLock(Mutex<()>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftInfo {
    id: String,
    modified_at: u64,
    preview: String,
}

fn roots(app: &AppHandle) -> Vec<PathBuf> {
    let install = std::env::current_exe().ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("Temp")));
    install.into_iter().chain(app.path().app_data_dir().ok().map(|dir| dir.join("Temp"))).collect()
}

fn valid_id(id: &str) -> bool {
    id.len() < 64 && id.starts_with("new-") && id.strip_suffix(".md")
        .is_some_and(|stem| stem.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

fn snapshots(dir: &Path) -> impl Iterator<Item = (u64, PathBuf)> {
    let entries = fs::symlink_metadata(dir).ok().filter(|m| m.is_dir())
        .and_then(|_| fs::read_dir(dir).ok());
    entries.into_iter().flatten().filter_map(|entry| {
        let entry = entry.ok()?;
        if !entry.file_type().ok()?.is_file() { return None; }
        let name = entry.file_name();
        let timestamp = name.to_str()?.strip_prefix("tmp")?.strip_suffix(".md")?.parse().ok()?;
        Some((timestamp, entry.path()))
    })
}

fn latest(app: &AppHandle, id: &str) -> Option<(u64, PathBuf)> {
    roots(app).into_iter().flat_map(|root| snapshots(&root.join(id))).max_by_key(|(time, _)| *time)
}

#[tauri::command]
pub fn draft_list(app: AppHandle, lock: State<'_, DraftLock>) -> Vec<DraftInfo> {
    let _guard = lock.0.lock().unwrap();
    let mut ids = std::collections::BTreeSet::new();
    for root in roots(&app) {
        for entry in fs::read_dir(root).into_iter().flatten().flatten() {
            if let Ok(id) = entry.file_name().into_string() {
                if valid_id(&id) { ids.insert(id); }
            }
        }
    }
    ids.into_iter().filter_map(|id| {
        let (modified_at, path) = latest(&app, &id)?;
        let mut text = Vec::new();
        fs::File::open(path).ok()?.take(1024).read_to_end(&mut text).ok()?;
        Some(DraftInfo { id, modified_at, preview: String::from_utf8_lossy(&text).into_owned() })
    }).collect()
}

#[tauri::command]
pub fn draft_read(app: AppHandle, lock: State<'_, DraftLock>, id: String) -> Result<String, String> {
    let _guard = lock.0.lock().unwrap();
    if !valid_id(&id) { return Err("草稿编号无效".into()); }
    let (_, path) = latest(&app, &id).ok_or("草稿文件不存在")?;
    fs::read_to_string(path).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn draft_save(app: AppHandle, lock: State<'_, DraftLock>, id: String, content: String, modified_at: u64) -> Result<u64, String> {
    let _guard = lock.0.lock().unwrap();
    if !valid_id(&id) { return Err("草稿编号无效".into()); }
    let timestamp = latest(&app, &id).map_or(modified_at, |(previous, _)| modified_at.max(previous.saturating_add(1)));
    let mut failure = "无法找到草稿目录".to_string();
    for root in roots(&app) {
        let dir = root.join(&id);
        let result = (|| -> std::io::Result<()> {
            fs::create_dir_all(&dir)?;
            if !fs::symlink_metadata(&dir)?.is_dir() {
                return Err(std::io::Error::new(ErrorKind::InvalidInput, "草稿目录无效"));
            }
            let pending = dir.join(format!("tmp{timestamp}.pending"));
            let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&pending)?;
            use std::io::Write;
            let write_result = file.write_all(content.as_bytes()).and_then(|_| file.sync_all());
            drop(file);
            let commit = write_result.and_then(|_| fs::rename(&pending, dir.join(format!("tmp{timestamp}.md"))));
            if commit.is_err() { let _ = fs::remove_file(pending); }
            commit
        })();
        match result {
            Ok(()) => {
                for (time, path) in snapshots(&dir) {
                    if time < timestamp { let _ = fs::remove_file(path); }
                }
                return Ok(timestamp);
            }
            Err(error) => failure = error.to_string(),
        }
    }
    Err(failure)
}

#[tauri::command]
pub fn draft_remove(app: AppHandle, lock: State<'_, DraftLock>, id: String) -> Result<(), String> {
    let _guard = lock.0.lock().unwrap();
    if !valid_id(&id) { return Err("草稿编号无效".into()); }
    for root in roots(&app) {
        let dir = root.join(&id);
        for (_, path) in snapshots(&dir) {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        let _ = fs::remove_dir(dir);
    }
    Ok(())
}
