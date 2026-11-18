//! Size limit and manual clearing for the web caches of every messenger and
//! shortcut data store.
//!
//! Telegram Web keeps the media it has shown (photos, video chunks, stickers,
//! avatars) in its Cache API storage and never evicts it. On the machine this
//! was written for that one origin had grown to 4.8 GB in ~40 000 files — 2 GB
//! of them in a cache Telegram had already deleted, which WebKit no longer
//! lists. Telegram's service worker opens that storage before the page loads,
//! so a cold start went from ~2 s to ~5.5 s until the chat list appeared.
//!
//! Clearing goes through `-[WKWebsiteDataStore removeDataOfTypes:modifiedSince:]`
//! with only the on-disk cache types — Cache API and the HTTP disk cache.
//! Cookies, local storage, IndexedDB and service worker registrations stay, so
//! every session stays signed in; the cost is re-downloading media when shown.
//! It is safe with the webview open (measured: 4.9 GB → 0 in ~4 s, the orphaned
//! cache included, Telegram still signed in), so neither the background check
//! nor the button needs a relaunch.
//!
//! WebKit reports no sizes, so the size is read from the store's directory.
//! That read is best-effort: a layout change only makes it report less.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_store::{Store, StoreExt};

use crate::{
    custom_webview_label, dialog_window, format_data_store_id, shortcut_id_to_data_store_id,
    CustomShortcuts, DataStoreError, UserMessengers, DATA_STORE_OP_TIMEOUT, MESSENGERS, SETTINGS_STORE,
};

/// Store key of the per-data-store limit, in megabytes.
const CACHE_LIMIT_KEY: &str = "cache_limit_mb";
const DEFAULT_CACHE_LIMIT_MB: u64 = 1024;
const MIN_CACHE_LIMIT_MB: u64 = 100;
const MAX_CACHE_LIMIT_MB: u64 = 100 * 1024;

/// Cache directories at the root of a store.
const ROOT_CACHE_DIRS: [&str; 3] = ["NetworkCache", "MediaCache", "CacheStorage"];
/// Per-origin Cache API directory: `Origins/<top origin>/<frame origin>/CacheStorage`.
const ORIGIN_CACHE_DIR: &str = "CacheStorage";

/// The first check waits until the startup preload has had its turn: the point
/// of the limit is a faster launch, so it must not compete with one.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(60);
/// Signalist runs for days without a restart, and the cache keeps growing.
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// How often the watch thread looks at the wall clock. `thread::sleep` runs on
/// a clock that stops while the Mac sleeps, so one 6-hour sleep could stretch
/// to days on a laptop; short naps measured against `SystemTime` cannot.
const WATCH_TICK: Duration = Duration::from_secs(60);

pub struct CacheLimit(pub Mutex<u64>);

impl Default for CacheLimit {
    fn default() -> Self {
        CacheLimit(Mutex::new(DEFAULT_CACHE_LIMIT_MB))
    }
}

/// Serializes clears: the periodic check and the button must not race on the
/// same store, and a second click while one runs has nothing left to do.
static CLEARING: Mutex<()> = Mutex::new(());

/// Reads the saved limit; anything missing or out of range falls back to the default.
pub fn load_cache_limit(store: &Store<tauri::Wry>) -> u64 {
    store
        .get(CACHE_LIMIT_KEY)
        .and_then(|v| v.as_u64())
        .filter(|mb| (MIN_CACHE_LIMIT_MB..=MAX_CACHE_LIMIT_MB).contains(mb))
        .unwrap_or(DEFAULT_CACHE_LIMIT_MB)
}

/// One data store the app owns, named the way the sidebar names it.
struct StoreEntry {
    name: String,
    id: [u8; 16],
    /// The webview label of a shortcut or user messenger; `None` for a
    /// built-in, whose store is never removed.
    custom_label: Option<String>,
}

/// Built-ins, then user messengers, then shortcuts — the sidebar order.
fn known_stores(app: &AppHandle) -> Vec<StoreEntry> {
    let mut entries: Vec<StoreEntry> = MESSENGERS
        .iter()
        .map(|m| StoreEntry { name: m.display_name.to_string(), id: m.data_store_id, custom_label: None })
        .collect();
    if let Some(state) = app.try_state::<UserMessengers>() {
        entries.extend(state.0.lock().unwrap().iter().map(|m| StoreEntry {
            name: m.name.clone(),
            id: shortcut_id_to_data_store_id(m.id.as_str()),
            custom_label: Some(custom_webview_label(m.id.as_str())),
        }));
    }
    if let Some(state) = app.try_state::<CustomShortcuts>() {
        entries.extend(state.0.lock().unwrap().iter().map(|s| StoreEntry {
            name: s.name.clone(),
            id: shortcut_id_to_data_store_id(s.id.as_str()),
            custom_label: Some(custom_webview_label(s.id.as_str())),
        }));
    }
    entries
}

/// Candidate names of this process's folder under `~/Library/WebKit`, most
/// likely first.
///
/// WebKit uses the main bundle's identifier, and the process name when there
/// is no bundle. `npm run tauri:dev` runs the binary unbundled, so its stores
/// live under `~/Library/WebKit/signalist` — not under the dev identifier from
/// `tauri.dev.conf.json`, which only a bundled `.app` would use.
fn webkit_app_dir_names() -> Vec<String> {
    let mut names = Vec::new();
    if let Some(id) = objc2_foundation::NSBundle::mainBundle().bundleIdentifier() {
        names.push(id.to_string());
    }
    if let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
    {
        if !names.contains(&exe) {
            names.push(exe);
        }
    }
    names
}

/// The `WebsiteDataStore` directory WebKit really uses for this app, or `None`
/// while none of the candidates exists.
///
/// Resolved against the disk instead of trusted: a wrong guess would make every
/// store measure 0 and the limit would silently never apply. Cached only once
/// found — before the first webview loads there may be nothing to find yet.
fn data_store_root() -> Option<PathBuf> {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    if let Some(root) = ROOT.get() {
        return Some(root.clone());
    }
    let webkit = PathBuf::from(std::env::var_os("HOME")?).join("Library/WebKit");
    let root = webkit_app_dir_names()
        .into_iter()
        .map(|name| webkit.join(name).join("WebsiteDataStore"))
        .find(|dir| dir.is_dir())?;
    Some(ROOT.get_or_init(|| root).clone())
}

/// Where WebKit keeps an identifier-keyed store of this app. Read-only use.
fn store_dir(root: &Path, id: [u8; 16]) -> PathBuf {
    root.join(format_data_store_id(id))
}

/// Bytes on disk under `path`, the way `du` counts them. Symlinks are not followed.
fn disk_usage(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    let own = meta.blocks() * 512;
    if !meta.is_dir() {
        return own;
    }
    let Ok(entries) = std::fs::read_dir(path) else { return own };
    own + entries.flatten().map(|e| disk_usage(&e.path())).sum::<u64>()
}

/// Bytes of cache data in one store: the caches at the store root plus every
/// origin's Cache API storage.
fn cache_bytes(root: &Path, id: [u8; 16]) -> u64 {
    let store = store_dir(root, id);
    let mut total: u64 = ROOT_CACHE_DIRS.iter().map(|d| disk_usage(&store.join(d))).sum();
    if let Ok(tops) = std::fs::read_dir(store.join("Origins")) {
        for top in tops.flatten() {
            if let Ok(frames) = std::fs::read_dir(top.path()) {
                total += frames
                    .flatten()
                    .map(|f| disk_usage(&f.path().join(ORIGIN_CACHE_DIR)))
                    .sum::<u64>();
            }
        }
    }
    total
}

/// Removes the on-disk cache types of one store through WebKit.
///
/// `WKWebsiteDataTypeMemoryCache` is left out on purpose: it frees no disk,
/// and purging it makes WebKit wait on every WebContent process. With the
/// hidden WhatsApp view that answer never came — the files were already gone,
/// yet the completion handler did not run within the timeout.
///
/// A shortcut or user messenger is cleared only while its webview exists, and
/// that is checked on the main thread right before the store object is
/// obtained. `teardown_custom_entry` runs on the main thread too and removes a
/// store immediately only for an entry with *no* webview, relying on nothing
/// holding it; `dataStoreForIdentifier:` here would hold it (until WebKit
/// answers — forever after a timeout) and could even re-create a store being
/// purged. With the check, an entry torn down first is skipped, and one torn
/// down afterwards had a webview, so its store is left to the next launch's
/// sweep. Built-in stores are never removed and need no check.
///
/// Main-thread only, hence the hop; **never call it from the main thread** —
/// it blocks on a completion handler delivered there (the trap
/// `remove_data_store` documents).
fn clear_store_cache(app: &AppHandle, entry: &StoreEntry) -> Result<ClearOutcome, DataStoreError> {
    use objc2_foundation::{NSDate, NSSet, NSString, NSUUID};
    use objc2_web_kit::{WKWebsiteDataStore, WKWebsiteDataTypeDiskCache, WKWebsiteDataTypeFetchCache};

    let id = entry.id;
    let custom_label = entry.custom_label.clone();
    let main_app = app.clone();
    let (tx, rx) = std::sync::mpsc::channel::<Result<ClearOutcome, String>>();
    let dispatched = app.run_on_main_thread(move || {
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            let _ = tx.send(Err("not on the main thread".into()));
            return;
        };
        if let Some(label) = custom_label {
            if main_app.get_webview(&label).is_none() {
                let _ = tx.send(Ok(ClearOutcome::NotOpen));
                return;
            }
        }
        let store = unsafe { WKWebsiteDataStore::dataStoreForIdentifier(&NSUUID::from_bytes(id), mtm) };
        let types: [&NSString; 2] = unsafe { [WKWebsiteDataTypeFetchCache, WKWebsiteDataTypeDiskCache] };
        let types = NSSet::from_slice(&types);
        // The handler keeps the store alive until the removal has finished.
        let held = store.clone();
        let handler = block2::RcBlock::new(move || {
            let _ = &held;
            let _ = tx.send(Ok(ClearOutcome::Cleared));
        });
        // The Unix epoch, not `distantPast`: it is what Apple's own samples
        // pass, and it already predates anything a store can hold.
        let since = NSDate::dateWithTimeIntervalSince1970(0.0);
        unsafe { store.removeDataOfTypes_modifiedSince_completionHandler(&types, &since, &handler) };
    });
    if let Err(e) = dispatched {
        return Err(DataStoreError::Failed(e.to_string()));
    }
    match rx.recv_timeout(DATA_STORE_OP_TIMEOUT) {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(message)) => Err(DataStoreError::Failed(message)),
        Err(_) => Err(DataStoreError::TimedOut),
    }
}

enum ClearOutcome {
    Cleared,
    /// A shortcut or user messenger without a webview: left alone, see
    /// `clear_store_cache`. Its cache does not grow while it is closed, and
    /// the first check after it is opened trims it.
    NotOpen,
}

/// Clears every store whose cache exceeds `limit_bytes` (every non-empty one
/// with `None`). Returns the bytes freed.
fn clear_stores(app: &AppHandle, limit_bytes: Option<u64>) -> u64 {
    let _guard = CLEARING.lock().unwrap_or_else(|e| e.into_inner());
    let Some(root) = data_store_root() else {
        log::warn!(
            "Cache limit not applied: no WebKit data-store folder found for any of {:?}",
            webkit_app_dir_names()
        );
        return 0;
    };
    let mut freed = 0u64;
    for entry in known_stores(app) {
        let before = cache_bytes(&root, entry.id);
        // Nothing on disk may also mean the store does not exist yet, and
        // `dataStoreForIdentifier:` would create it.
        if before == 0 || limit_bytes.is_some_and(|limit| before <= limit) {
            continue;
        }
        let started = Instant::now();
        let result = clear_store_cache(app, &entry);
        if matches!(result, Ok(ClearOutcome::NotOpen)) {
            log::info!(
                "Skipped {} cache ({} MB): not open in this session",
                entry.name,
                before / (1024 * 1024)
            );
            continue;
        }
        // Measured whatever WebKit answered: a timeout means only that the
        // answer did not arrive, and the files may well be gone already.
        let after = cache_bytes(&root, entry.id);
        freed += before.saturating_sub(after);
        let sizes = format!(
            "{} MB -> {} MB in {} ms",
            before / (1024 * 1024),
            after / (1024 * 1024),
            started.elapsed().as_millis()
        );
        match result {
            Ok(_) => log::info!("Cleared {} cache: {}", entry.name, sizes),
            Err(e) => log::warn!("Clearing {} cache unconfirmed ({}): {}", entry.name, e, sizes),
        }
    }
    freed
}

/// Periodic enforcement of the limit, on its own thread: it walks directory
/// trees and blocks on WebKit callbacks.
pub fn spawn_cache_limit_watch(app: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_CHECK_DELAY);
        loop {
            let limit = *app.state::<CacheLimit>().0.lock().unwrap() * 1024 * 1024;
            clear_stores(&app, Some(limit));
            // Wall-clock time, so the hours the Mac spent asleep count too. A
            // clock set backwards reads as an error and triggers the check.
            let checked_at = SystemTime::now();
            loop {
                std::thread::sleep(WATCH_TICK);
                match checked_at.elapsed() {
                    Ok(elapsed) if elapsed < CHECK_INTERVAL => continue,
                    _ => break,
                }
            }
        }
    });
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheEntryInfo {
    /// The data-store UUID: unique, unlike display names.
    id: String,
    name: String,
    bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheInfo {
    limit_mb: u64,
    min_limit_mb: u64,
    max_limit_mb: u64,
    entries: Vec<CacheEntryInfo>,
}

/// `async` so the directory walk runs off the main thread.
#[tauri::command]
pub async fn get_cache_info(app: AppHandle) -> Result<CacheInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = data_store_root();
        CacheInfo {
            limit_mb: *app.state::<CacheLimit>().0.lock().unwrap(),
            min_limit_mb: MIN_CACHE_LIMIT_MB,
            max_limit_mb: MAX_CACHE_LIMIT_MB,
            entries: known_stores(&app)
                .into_iter()
                .map(|e| CacheEntryInfo {
                    id: format_data_store_id(e.id),
                    bytes: root.as_deref().map_or(0, |root| cache_bytes(root, e.id)),
                    name: e.name,
                })
                .collect(),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// Saves a new limit. The next periodic check applies it — lowering the limit
/// does not clear anything while the user is still typing.
#[tauri::command]
pub fn set_cache_limit(app: AppHandle, limit_mb: u64) -> Result<(), String> {
    if !(MIN_CACHE_LIMIT_MB..=MAX_CACHE_LIMIT_MB).contains(&limit_mb) {
        return Err(format!(
            "Limit must be between {} MB and {} MB",
            MIN_CACHE_LIMIT_MB, MAX_CACHE_LIMIT_MB
        ));
    }
    *app.state::<CacheLimit>().0.lock().unwrap() = limit_mb;
    let store = app.store(SETTINGS_STORE).map_err(|e| e.to_string())?;
    store.set(CACHE_LIMIT_KEY, limit_mb);
    store.save().map_err(|e| e.to_string())
}

/// Clears every cache now and returns the bytes freed. `async` plus
/// `spawn_blocking`: a plain command runs on the main thread, where the WebKit
/// completion handler has to be delivered.
#[tauri::command]
pub async fn clear_caches_now(app: AppHandle) -> Result<u64, String> {
    tauri::async_runtime::spawn_blocking(move || clear_stores(&app, None))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn open_cache_window(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("cache") {
        let _ = win.set_focus();
        return Ok(());
    }
    dialog_window(&app, "cache", "index.html?view=cache".into(), "Cache", 420.0, 460.0)
        .min_inner_size(360.0, 360.0)
        .resizable(true)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}
