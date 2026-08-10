use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use serde::{Serialize, Deserialize};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    path::BaseDirectory,
    tray::TrayIconBuilder,
    webview::WebviewBuilder, AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, State,
    RunEvent, WebviewUrl, WebviewWindowBuilder, WindowBuilder, WindowEvent,
};
use tauri::utils::config::BackgroundThrottlingPolicy;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tauri_plugin_store::StoreExt;
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
#[cfg(target_os = "macos")]
use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial};

const SIDEBAR_WIDTH: f64 = 64.0;

// How long the unread count must remain stable before we post a notification.
// Coalesces rapid changes during Telegram's message-sync bursts so the value
// shown in the macOS Notification Center reflects the settled count, not a
// transient mid-sync spike that the sidebar later overwrites.
const NOTIFY_DEBOUNCE_MS: u64 = 800;

const CHROME_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

// Safari UA for custom shortcuts: matches WKWebView's actual TLS fingerprint,
// accepted by Google OAuth and Cloudflare bot checks.
const SAFARI_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.3 Safari/605.1.15";

struct Messenger {
    label: &'static str,
    display_name: &'static str,
    url: &'static str,
    allowed_domains: &'static [&'static str],
    data_store_id: [u8; 16],
}

const MESSENGERS: &[Messenger] = &[
    Messenger {
        label: "telegram",
        display_name: "Telegram",
        url: "https://web.telegram.org/a/",
        allowed_domains: &["web.telegram.org", "t.me"],
        data_store_id: [
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x01,
        ],
    },
    Messenger {
        label: "whatsapp",
        display_name: "WhatsApp",
        url: "https://web.whatsapp.com/",
        allowed_domains: &["web.whatsapp.com", "whatsapp.com", "whatsapp.net"],
        data_store_id: [
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x02,
        ],
    },
];

pub struct ActiveMessenger(pub Mutex<String>);

#[derive(Default)]
pub struct UnreadCounts(pub Mutex<HashMap<String, u32>>);

#[derive(Default, Clone, Copy)]
pub struct NotifyState {
    // Highest count we have already shown in the Notification Center for this
    // messenger. Reset to the current value when count drops, so the next rise
    // is treated as a fresh notification trigger.
    last_notified: u32,
    // Monotonic generation. Bumped on every change; the single in-flight
    // debounce thread re-reads it after each sleep and either fires (if stable)
    // or restarts the sleep (if a newer change arrived).
    pending_gen: u64,
    // True while a debounce thread is sleeping for this messenger. Prevents
    // spawning a fresh OS thread on every burst event — the in-flight thread
    // simply sleeps another window if pending_gen advanced.
    in_flight: bool,
}

#[derive(Default)]
pub struct NotifyTracker(pub Mutex<HashMap<String, NotifyState>>);

pub struct HotkeyConfig(pub Mutex<String>);

pub struct DockHidden(pub Mutex<bool>);

pub struct SilenceMode(pub Mutex<bool>);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomShortcut {
    pub id: String,
    pub name: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Tint applied to the sidebar icon, as `#rrggbb`. `None` follows the theme accent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

impl CustomShortcut {
    fn webview_label(&self) -> String {
        custom_webview_label(&self.id)
    }
}

fn custom_webview_label(id: &str) -> String {
    format!("custom-{}", id)
}

fn is_custom_label(label: &str) -> bool {
    label.starts_with("custom-")
}

/// Accepts only `#rrggbb`; the value is bound straight into a CSS `color`
/// property in the sidebar, so anything else is rejected rather than sanitized.
/// Empty/blank input means "no custom color".
fn normalize_color(color: Option<String>) -> Result<Option<String>, String> {
    let Some(raw) = color else { return Ok(None) };
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        return Ok(None);
    }
    let is_hex6 = value.len() == 7
        && value.starts_with('#')
        && value[1..].chars().all(|c| c.is_ascii_hexdigit());
    if is_hex6 {
        Ok(Some(value))
    } else {
        Err(format!("Invalid color: {} (expected #rrggbb)", raw))
    }
}

fn content_bounds(window_logical: LogicalSize<f64>) -> (LogicalPosition<f64>, LogicalSize<f64>) {
    (
        LogicalPosition::new(SIDEBAR_WIDTH, 0.0),
        LogicalSize::new(window_logical.width - SIDEBAR_WIDTH, window_logical.height),
    )
}

pub struct CustomShortcuts(pub Mutex<Vec<CustomShortcut>>);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessenger {
    pub id: String,
    pub name: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Create this webview at app startup instead of on first click. Costs one
    /// background WebContent process; see `preload_at_startup`.
    #[serde(default)]
    pub preload: bool,
}

impl UserMessenger {
    fn webview_label(&self) -> String {
        custom_webview_label(&self.id)
    }
}

pub struct UserMessengers(pub Mutex<Vec<UserMessenger>>);

/// Per-label preload flag for the built-in messengers. Absent label means the
/// default (`true`) — a fresh install loads Telegram and WhatsApp at startup so
/// their inject scripts report unread counts without being opened first.
#[derive(Default)]
pub struct BuiltinPreload(pub Mutex<HashMap<String, bool>>);

fn builtin_preload_enabled(app: &AppHandle, label: &str) -> bool {
    app.try_state::<BuiltinPreload>()
        .and_then(|s| s.0.lock().unwrap().get(label).copied())
        .unwrap_or(true)
}

fn persist_builtin_preload(app: &AppHandle) -> Result<(), String> {
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    let flags = app.state::<BuiltinPreload>().0.lock().unwrap().clone();
    let json = serde_json::to_value(&flags).map_err(|e| e.to_string())?;
    store.set("builtin_preload", json);
    store.save().map_err(|e| e.to_string())
}

fn is_google_domain(domain: &str) -> bool {
    let d = domain.trim_start_matches("www.").to_ascii_lowercase();
    d == "google.com"
        || d.ends_with(".google.com")
        || d == "googleusercontent.com"
        || d.ends_with(".googleusercontent.com")
        || d == "googleapis.com"
        || d.ends_with(".googleapis.com")
        || d == "youtube.com"
        || d.ends_with(".youtube.com")
}

fn open_in_chrome(url: &str) {
    let _ = std::process::Command::new("open")
        .arg("-a")
        .arg("Google Chrome")
        .arg(url)
        .spawn();
}

#[tauri::command]
fn open_in_browser(url: String) -> Result<(), String> {
    let parsed: tauri::Url = url.parse().map_err(|_| "Invalid URL".to_string())?;
    if !matches!(parsed.scheme(), "https" | "http") {
        return Err("Only HTTP/HTTPS URLs are supported".into());
    }
    open_in_chrome(&url);
    Ok(())
}

fn shortcut_id_to_data_store_id(shortcut_id: &str) -> [u8; 16] {
    let mut result = [0u8; 16];
    for (i, chunk) in shortcut_id.as_bytes().chunks(2).take(8).enumerate() {
        if let Ok(s) = std::str::from_utf8(chunk) {
            result[i] = u8::from_str_radix(s, 16).unwrap_or(0);
        }
    }
    result
}

fn generate_shortcut_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    format!("{:08x}{:08x}", d.as_secs(), d.subsec_nanos())
}

fn persist_custom_shortcuts(app: &AppHandle) -> Result<(), String> {
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    let shortcuts = app.state::<CustomShortcuts>().0.lock().unwrap().clone();
    let json = serde_json::to_value(&shortcuts).map_err(|e| e.to_string())?;
    store.set("custom_shortcuts", json);
    store.save().map_err(|e| e.to_string())
}

fn persist_user_messengers(app: &AppHandle) -> Result<(), String> {
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    let messengers = app.state::<UserMessengers>().0.lock().unwrap().clone();
    let json = serde_json::to_value(&messengers).map_err(|e| e.to_string())?;
    store.set("user_messengers", json);
    store.save().map_err(|e| e.to_string())
}

#[derive(Clone, Serialize)]
struct UnreadUpdatePayload {
    messenger: String,
    count: u32,
}

fn build_tray_menu(app: &AppHandle) -> Option<Menu<tauri::Wry>> {
    let counts = app.state::<UnreadCounts>().0.lock().unwrap().clone();
    let hotkey = app.state::<HotkeyConfig>().0.lock().unwrap().clone();
    let dock_hidden = *app.state::<DockHidden>().0.lock().unwrap();

    let menu = Menu::new(app).ok()?;

    for m in MESSENGERS {
        let count = counts.get(m.label).copied().unwrap_or(0);
        let dot = if count > 0 { "◉" } else { "○" };
        let label = format!("{}  {}", dot, m.display_name);
        if let Ok(item) = MenuItem::with_id(app, m.label, &label, true, None::<&str>) {
            let _ = menu.append(&item);
        }
    }

    if let Some(state) = app.try_state::<UserMessengers>() {
        let entries: Vec<(String, String)> = state.0.lock().unwrap()
            .iter()
            .map(|m| (m.webview_label(), m.name.clone()))
            .collect();
        for (webview_label, name) in &entries {
            let label = format!("○  {}", name);
            if let Ok(item) = MenuItem::with_id(app, webview_label, &label, true, None::<&str>) {
                let _ = menu.append(&item);
            }
        }
    }

    if let Some(state) = app.try_state::<CustomShortcuts>() {
        let shortcuts: Vec<(String, String)> = state.0.lock().unwrap()
            .iter()
            .map(|sc| (sc.webview_label(), sc.name.clone()))
            .collect();
        for (webview_label, name) in &shortcuts {
            let label = format!("○  {}", name);
            if let Ok(item) = MenuItem::with_id(app, webview_label, &label, true, None::<&str>) {
                let _ = menu.append(&item);
            }
        }
    }

    if let Ok(sep) = PredefinedMenuItem::separator(app) { let _ = menu.append(&sep); }

    let accel = hotkey.replace("Super", "Cmd");
    if let Ok(toggle_item) = MenuItem::with_id(app, "toggle_window", "⧉  Show/Hide", true, Some(accel.as_str())) {
        let _ = menu.append(&toggle_item);
    }

    if let Ok(sep) = PredefinedMenuItem::separator(app) { let _ = menu.append(&sep); }

    let dock_label = if dock_hidden { "▭  Show in Dock" } else { "▭  Hide in Dock" };
    if let Ok(dock_item) = MenuItem::with_id(app, "toggle_dock", dock_label, true, None::<&str>) {
        let _ = menu.append(&dock_item);
    }

    if let Ok(sep) = PredefinedMenuItem::separator(app) { let _ = menu.append(&sep); }

    if let Ok(quit_item) = MenuItem::with_id(app, "quit", "⏻  Quit", true, Some("Cmd+Q")) {
        let _ = menu.append(&quit_item);
    }

    Some(menu)
}

fn update_tray(app: &AppHandle) {
    let Some(tray) = app.tray_by_id("main-tray") else { return };
    let Some(menu) = build_tray_menu(app) else { return };
    let _ = tray.set_menu(Some(menu));
    let total: u32 = app.state::<UnreadCounts>().0.lock().unwrap().values().sum();
    // template=true → dim (standard menu bar), template=false → bright (full color, visually active)
    let _ = tray.set_icon_as_template(total == 0);
}

fn do_toggle_dock_icon(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let new_hidden = {
            let state = app.state::<DockHidden>();
            let mut hidden = state.0.lock().unwrap();
            *hidden = !*hidden;
            *hidden
        };
        if new_hidden {
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
        } else {
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
        }
        update_tray(app);
    }
}

#[tauri::command]
fn update_sidebar_theme_from_webview(app: AppHandle, is_dark: bool) {
    let _ = app.emit("theme-update", is_dark);
}

#[tauri::command]
fn update_unread_count(app: AppHandle, messenger: String, count: u32) {
    if !MESSENGERS.iter().any(|m| m.label == messenger.as_str()) {
        log::warn!("[update_unread_count] REJECTED unknown messenger: {}", messenger);
        return;
    }
    let count = count.min(10_000);
    log::debug!("[update_unread_count] CALLED for {} with count {}", messenger, count);

    let Some(unread_state) = app.try_state::<UnreadCounts>() else { return };
    let previous_count = {
        let mut map = unread_state.0.lock().unwrap();
        let prev = map.get(&messenger).copied().unwrap_or(0);
        if prev == count {
            return;
        }
        map.insert(messenger.clone(), count);
        prev
    };

    update_tray(&app);
    let _ = app.emit("unread-update", UnreadUpdatePayload { messenger: messenger.clone(), count });

    handle_notify_change(&app, &messenger, count, previous_count);
}

// Decide whether the change calls for a (debounced) notification, then either
// schedule one or update the baseline accordingly.
//
// Behaviour matrix:
//   count == 0 OR count < previous   → drop is genuine: cancel any pending
//                                       notification and lower the baseline,
//                                       no notification fired.
//   count <= last_notified            → we already announced this value;
//                                       nothing to do.
//   otherwise (count rose)            → bump generation, spawn a debounce
//                                       task. If another change arrives within
//                                       the debounce window, the gen mismatch
//                                       cancels the in-flight task and a new
//                                       one is scheduled with the latest value.
fn handle_notify_change(app: &AppHandle, messenger: &str, count: u32, previous_count: u32) {
    let Some(tracker) = app.try_state::<NotifyTracker>() else { return };
    let mut map = tracker.0.lock().unwrap();
    let entry = map.entry(messenger.to_string()).or_default();

    if count == 0 || count < previous_count {
        entry.last_notified = count;
        entry.pending_gen = entry.pending_gen.wrapping_add(1);
        return;
    }

    if count <= entry.last_notified {
        return;
    }

    entry.pending_gen = entry.pending_gen.wrapping_add(1);
    if entry.in_flight {
        // A debounce thread is already sleeping for this messenger; it will
        // observe the bumped pending_gen on wake and restart its sleep window.
        return;
    }
    entry.in_flight = true;
    drop(map);

    let app_clone = app.clone();
    let messenger_clone = messenger.to_string();
    std::thread::spawn(move || {
        let read_gen = || -> Option<u64> {
            app_clone.try_state::<NotifyTracker>().map(|t| {
                t.0.lock().unwrap().get(&messenger_clone).map(|s| s.pending_gen).unwrap_or(0)
            })
        };
        loop {
            let Some(gen_at_sleep) = read_gen() else { return };
            std::thread::sleep(Duration::from_millis(NOTIFY_DEBOUNCE_MS));
            let Some(gen_now) = read_gen() else { return };
            if gen_now == gen_at_sleep {
                break;
            }
        }
        fire_notification_if_stable(&app_clone, &messenger_clone);
        if let Some(tracker) = app_clone.try_state::<NotifyTracker>() {
            if let Some(entry) = tracker.0.lock().unwrap().get_mut(&messenger_clone) {
                entry.in_flight = false;
            }
        }
    });
}

// Runs after the debounce window has settled (no further pending_gen changes).
// Reads the current count from UnreadCounts and fires a notification only if
// the value is still above the last announced one. Updates last_notified
// atomically so a focused window doesn't re-trigger the same notification once
// it loses focus.
fn fire_notification_if_stable(app: &AppHandle, messenger: &str) {
    let current_count = match app.try_state::<UnreadCounts>() {
        Some(state) => state.0.lock().unwrap().get(messenger).copied().unwrap_or(0),
        None => return,
    };
    if current_count == 0 {
        return;
    }

    let display_name = MESSENGERS
        .iter()
        .find(|m| m.label == messenger)
        .map(|c| c.display_name.to_string())
        .unwrap_or_else(|| messenger.to_string());

    if app.try_state::<SilenceMode>().map(|s| *s.0.lock().unwrap()).unwrap_or(false) {
        return;
    }
    let window_focused = app
        .get_window("main")
        .and_then(|w| w.is_focused().ok())
        .unwrap_or(false);
    if window_focused {
        return;
    }

    {
        let Some(tracker) = app.try_state::<NotifyTracker>() else { return };
        let mut map = tracker.0.lock().unwrap();
        let entry = map.entry(messenger.to_string()).or_default();
        if current_count <= entry.last_notified {
            return;
        }
        entry.last_notified = current_count;
    }

    let body = if current_count == 1 {
        "You have 1 unread message".to_string()
    } else {
        format!("You have {} unread messages", current_count)
    };
    let icon_path = app
        .path()
        .resolve("icons/icon.png", BaseDirectory::Resource)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut builder = app
        .notification()
        .builder()
        .title(format!("New message on {}", display_name))
        .body(body);
    if !icon_path.is_empty() {
        builder = builder.icon(icon_path);
    }

    match builder.show() {
        Ok(()) => log::debug!("Notification sent for {} (count={})", display_name, current_count),
        Err(e) => log::error!("Notification failed for {}: {}", display_name, e),
    }
}

fn get_logical_size(window: &tauri::Window) -> Result<LogicalSize<f64>, String> {
    let physical = window.inner_size().map_err(|e| e.to_string())?;
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    Ok(physical.to_logical(scale))
}

fn reposition_webviews(app: &AppHandle) {
    let window = match app.get_window("main") {
        Some(w) => w,
        None => return,
    };

    let logical = match get_logical_size(&window) {
        Ok(s) => s,
        Err(_) => return,
    };

    if let Some(sidebar) = app.get_webview("sidebar") {
        let _ = sidebar.set_position(LogicalPosition::new(0.0, 0.0));
        let _ = sidebar.set_size(LogicalSize::new(SIDEBAR_WIDTH, logical.height));
    }

    let (pos, size) = content_bounds(logical);
    for m in MESSENGERS {
        if let Some(webview) = app.get_webview(m.label) {
            let _ = webview.set_position(pos);
            let _ = webview.set_size(size);
        }
    }
    // Hidden custom webviews stay at 0×0 to keep their IOSurface released
    // (see hide_all_messengers); only the active one is resized here.
    let active = app.state::<ActiveMessenger>().0.lock().unwrap().clone();
    if is_custom_label(&active) {
        if let Some(webview) = app.get_webview(&active) {
            let _ = webview.set_position(pos);
            let _ = webview.set_size(size);
        }
    }
}

#[tauri::command]
async fn open_messenger(app: AppHandle, messenger: String) -> Result<String, String> {
    ensure_messenger_webview(app, messenger, true).await
}

/// Creates the messenger webview if missing, then either brings it to the front
/// (`activate`) or leaves it hidden in the background. The hidden path is what
/// startup preloading uses: the page loads and its inject script starts
/// reporting unread counts without ever becoming the visible view.
async fn ensure_messenger_webview(
    app: AppHandle,
    messenger: String,
    activate: bool,
) -> Result<String, String> {
    let config = MESSENGERS
        .iter()
        .find(|m| m.label == messenger)
        .ok_or_else(|| format!("Unknown messenger: {}", messenger))?;

    if let Some(webview) = app.get_webview(config.label) {
        if !activate {
            return Ok(format!("{} already loaded", config.label));
        }
        hide_all_messengers(&app);
        webview.show().map_err(|e| e.to_string())?;
        webview.set_focus().map_err(|e| e.to_string())?;
        let state = app.state::<ActiveMessenger>();
        let mut active = state.0.lock().unwrap();
        *active = messenger.clone();
        drop(active);
        let _ = app.emit("active-messenger-changed", messenger.clone());
        return Ok(format!("Focused existing {}", config.label));
    }

    let window = app
        .get_window("main")
        .ok_or("Main window not found")?;

    let logical = get_logical_size(&window)?;
    let content_width = logical.width - SIDEBAR_WIDTH;
    let content_height = logical.height;

    let allowed_domains = config.allowed_domains.to_vec();
    let nav_guard = move |url: &tauri::Url| -> bool {
        if let Some(host) = url.host_str() {
            let is_allowed = allowed_domains
                .iter()
                .any(|d| host == *d || host.ends_with(&format!(".{}", d)));
            if !is_allowed && matches!(url.scheme(), "https" | "http") {
                open_in_chrome(url.as_str());
            }
            is_allowed
        } else {
            false
        }
    };

    let parsed_url: tauri::Url = config.url.parse().map_err(|e| format!("{}", e))?;

    let init_script = match config.label {
        "telegram" => include_str!("../inject/telegram.js"),
        "whatsapp" => include_str!("../inject/whatsapp.js"),
        _ => "",
    };

    let webview_builder = WebviewBuilder::new(config.label, WebviewUrl::External(parsed_url))
        .user_agent(CHROME_UA)
        .data_store_identifier(config.data_store_id)
        .on_navigation(nav_guard)
        .devtools(cfg!(debug_assertions))
        // Without this, WebKit throttles timers of a hidden view and fully
        // suspends it after ~5 minutes — the inject script would stop reporting
        // unread counts, which is exactly what background tracking needs.
        // macOS 14+; a no-op on older versions.
        .background_throttling(BackgroundThrottlingPolicy::Disabled)
        // Let the web app receive native HTML5 file drops (e.g. dragging a
        // screenshot into a chat). Tauri's own drag-drop handler otherwise
        // swallows the OS drop before it reaches the messenger's page.
        .disable_drag_drop_handler()
        .initialization_script(init_script);

    // A preloaded view must not steal focus from whatever is on screen.
    let webview_builder = if activate { webview_builder } else { webview_builder.focused(false) };

    if activate {
        hide_all_messengers(&app);
    }

    let child = window
        .add_child(
            webview_builder,
            LogicalPosition::new(SIDEBAR_WIDTH, 0.0),
            LogicalSize::new(content_width, content_height),
        )
        .map_err(|e| e.to_string())?;

    if !activate {
        // Built-in messengers keep their full size while hidden so the inject
        // script keeps tracking unread counts (same reasoning as
        // hide_all_messengers); only visibility is dropped.
        let _ = child.hide();
        return Ok(format!("Preloaded {}", config.label));
    }

    let state = app.state::<ActiveMessenger>();
    let mut active = state.0.lock().unwrap();
    *active = messenger.clone();
    drop(active);
    let _ = app.emit("active-messenger-changed", messenger.clone());

    Ok(format!("Created {}", config.label))
}

#[tauri::command]
fn switch_messenger(app: AppHandle, messenger: String) -> Result<(), String> {
    let config = MESSENGERS
        .iter()
        .find(|m| m.label == messenger)
        .ok_or_else(|| format!("Unknown messenger: {}", messenger))?;

    let webview = app.get_webview(config.label).ok_or_else(|| {
        format!("{} webview not created yet. Call open_messenger first.", messenger)
    })?;

    hide_all_messengers(&app);

    webview.show().map_err(|e| e.to_string())?;
    webview.set_focus().map_err(|e| e.to_string())?;

    let state = app.state::<ActiveMessenger>();
    let mut active = state.0.lock().unwrap();
    *active = messenger.clone();
    drop(active);
    let _ = app.emit("active-messenger-changed", messenger);

    Ok(())
}

#[tauri::command]
fn close_messenger(app: AppHandle, messenger: String) -> Result<(), String> {
    let config = MESSENGERS
        .iter()
        .find(|m| m.label == messenger)
        .ok_or_else(|| format!("Unknown messenger: {}", messenger))?;

    if let Some(webview) = app.get_webview(config.label) {
        webview.close().map_err(|e| e.to_string())?;
    }

    Ok(())
}

#[tauri::command]
fn get_active_messenger(app: AppHandle) -> Result<String, String> {
    let state = app.state::<ActiveMessenger>();
    let active = state.0.lock().unwrap();
    Ok(active.clone())
}

#[tauri::command]
async fn open_add_shortcut_window(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("add-shortcut") {
        let _ = win.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(
        &app,
        "add-shortcut",
        WebviewUrl::App("index.html?view=add-shortcut".into()),
    )
    .title("Add Web Shortcut")
    .inner_size(360.0, 560.0)
    .min_inner_size(360.0, 560.0)
    .resizable(false)
    .devtools(cfg!(debug_assertions))
    .center()
    .build()
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn open_edit_shortcut_window(app: AppHandle, id: String) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("edit-shortcut") {
        let _ = win.set_focus();
        return Ok(());
    }
    let url = format!("index.html?view=edit-shortcut&id={}", id);
    WebviewWindowBuilder::new(
        &app,
        "edit-shortcut",
        WebviewUrl::App(url.into()),
    )
    .title("Edit Web Shortcut")
    .inner_size(360.0, 490.0)
    .min_inner_size(360.0, 490.0)
    .resizable(false)
    .devtools(cfg!(debug_assertions))
    .center()
    .build()
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Confirmation prompt shown before a sidebar item is removed. Removal itself
/// stays in the sidebar (it also has to move the active view away from a
/// deleted item) — this window only reports the user's answer back over the
/// `confirm-delete-approved` event.
///
/// The label carries the target id so a second × click, while an earlier prompt
/// is still open, opens its own window instead of silently reusing one that
/// names a different item. Capabilities match it via the `confirm-delete-*`
/// glob, so the id is restricted to characters that are valid in a label.
#[tauri::command]
async fn open_confirm_delete_window(app: AppHandle, kind: String, id: String) -> Result<(), String> {
    if kind != "shortcut" && kind != "messenger" {
        return Err(format!("Unknown delete target kind: {}", kind));
    }
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("Invalid id".into());
    }
    let label = format!("confirm-delete-{}", id);
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.set_focus();
        return Ok(());
    }
    let url = format!("index.html?view=confirm-delete&kind={}&id={}", kind, id);
    WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(url.into()))
        .title(if kind == "shortcut" { "Delete Shortcut" } else { "Delete Messenger" })
        .inner_size(380.0, 190.0)
        .min_inner_size(380.0, 190.0)
        .resizable(false)
        .always_on_top(true)
        .devtools(cfg!(debug_assertions))
        .center()
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn update_custom_shortcut(
    app: AppHandle,
    id: String,
    name: String,
    url: String,
    icon: Option<String>,
    color: Option<String>,
) -> Result<CustomShortcut, String> {
    let parsed: tauri::Url = url.parse().map_err(|e| format!("Invalid URL: {}", e))?;
    let host = parsed.host_str().ok_or("URL has no host")?;
    if is_google_domain(host) {
        return Err("Google services (Gemini, Google, YouTube) are not supported in the embedded window due to Google's policy".into());
    }
    let color = normalize_color(color)?;

    let label = custom_webview_label(&id);
    let (updated, url_changed) = {
        let state = app.state::<CustomShortcuts>();
        let mut shortcuts = state.0.lock().unwrap();
        let sc = shortcuts.iter_mut().find(|s| s.id == id)
            .ok_or_else(|| format!("Shortcut not found: {}", id))?;
        let url_changed = sc.url != url;
        sc.name = name;
        sc.url = url;
        sc.icon = icon;
        sc.color = color;
        (sc.clone(), url_changed)
    };

    if url_changed {
        if let Some(webview) = app.get_webview(&label) {
            let _ = webview.close();
        }
    }

    persist_custom_shortcuts(&app)?;
    update_tray(&app);
    let _ = app.emit("shortcut-updated", updated.clone());
    Ok(updated)
}

#[tauri::command]
fn list_custom_shortcuts(app: AppHandle) -> Result<Vec<CustomShortcut>, String> {
    Ok(app.state::<CustomShortcuts>().0.lock().unwrap().clone())
}

#[tauri::command]
fn add_custom_shortcut(app: AppHandle, name: String, url: String, icon: Option<String>, color: Option<String>) -> Result<CustomShortcut, String> {
    let parsed: tauri::Url = url.parse().map_err(|e| format!("Invalid URL: {}", e))?;
    let host = parsed.host_str().ok_or("URL has no host")?;
    if is_google_domain(host) {
        return Err("Google services (Gemini, Google, YouTube) are not supported in the embedded window due to Google's policy".into());
    }
    let color = normalize_color(color)?;
    let sc = CustomShortcut { id: generate_shortcut_id(), name, url, icon, color };
    app.state::<CustomShortcuts>().0.lock().unwrap().push(sc.clone());
    persist_custom_shortcuts(&app)?;
    update_tray(&app);
    let _ = app.emit("shortcut-added", sc.clone());
    Ok(sc)
}

/// Reorders shortcuts to match `ids`. Order is purely presentational (sidebar
/// and tray menu both render the vec as-is), so nothing but persistence and the
/// tray needs refreshing — webviews keep their labels and data stores.
#[tauri::command]
fn reorder_custom_shortcuts(app: AppHandle, ids: Vec<String>) -> Result<Vec<CustomShortcut>, String> {
    let reordered = {
        let state = app.state::<CustomShortcuts>();
        let mut shortcuts = state.0.lock().unwrap();
        if ids.len() != shortcuts.len() {
            return Err(format!("Expected {} ids, got {}", shortcuts.len(), ids.len()));
        }
        // Only an exact permutation is accepted: a missing or duplicated id would
        // silently drop a shortcut and orphan its webview + data store.
        let mut remaining = shortcuts.clone();
        let mut picked = Vec::with_capacity(ids.len());
        for id in &ids {
            let pos = remaining
                .iter()
                .position(|sc| &sc.id == id)
                .ok_or_else(|| format!("Unknown or duplicated shortcut id: {}", id))?;
            picked.push(remaining.remove(pos));
        }
        *shortcuts = picked.clone();
        picked
    };

    persist_custom_shortcuts(&app)?;
    update_tray(&app);
    Ok(reordered)
}

#[tauri::command]
fn remove_custom_shortcut(app: AppHandle, id: String) -> Result<(), String> {
    let label = custom_webview_label(&id);
    if let Some(webview) = app.get_webview(&label) {
        webview.close().map_err(|e| e.to_string())?;
    }
    app.state::<CustomShortcuts>().0.lock().unwrap().retain(|sc| sc.id != id);
    persist_custom_shortcuts(&app)?;
    update_tray(&app);
    Ok(())
}

#[tauri::command]
fn list_user_messengers(app: AppHandle) -> Result<Vec<UserMessenger>, String> {
    Ok(app.state::<UserMessengers>().0.lock().unwrap().clone())
}

#[tauri::command]
fn add_user_messenger(
    app: AppHandle,
    name: String,
    url: String,
    icon: Option<String>,
    preload: Option<bool>,
) -> Result<UserMessenger, String> {
    let parsed: tauri::Url = url.parse().map_err(|e| format!("Invalid URL: {}", e))?;
    let host = parsed.host_str().ok_or("URL has no host")?;
    if is_google_domain(host) {
        return Err("Google services are not supported in the embedded window".into());
    }
    let m = UserMessenger {
        id: generate_shortcut_id(),
        name,
        url,
        icon,
        preload: preload.unwrap_or(false),
    };
    app.state::<UserMessengers>().0.lock().unwrap().push(m.clone());
    persist_user_messengers(&app)?;
    update_tray(&app);
    Ok(m)
}

/// Preload flags for the built-in messengers, one entry per label in MESSENGERS
/// (defaults filled in, so the sidebar never has to guess).
#[tauri::command]
fn get_builtin_preload(app: AppHandle) -> HashMap<String, bool> {
    MESSENGERS
        .iter()
        .map(|m| (m.label.to_string(), builtin_preload_enabled(&app, m.label)))
        .collect()
}

#[tauri::command]
fn set_builtin_preload(app: AppHandle, messenger: String, enable: bool) -> Result<(), String> {
    if !MESSENGERS.iter().any(|m| m.label == messenger) {
        return Err(format!("Unknown messenger: {}", messenger));
    }
    app.state::<BuiltinPreload>().0.lock().unwrap().insert(messenger, enable);
    persist_builtin_preload(&app)
}

/// Takes effect on the next launch only — an already-created webview is left
/// alone, and turning the flag on does not load the page mid-session.
#[tauri::command]
fn set_user_messenger_preload(app: AppHandle, id: String, enable: bool) -> Result<(), String> {
    {
        let state = app.state::<UserMessengers>();
        let mut list = state.0.lock().unwrap();
        let entry = list
            .iter_mut()
            .find(|m| m.id == id)
            .ok_or_else(|| format!("Unknown messenger: {}", id))?;
        entry.preload = enable;
    }
    persist_user_messengers(&app)
}

#[tauri::command]
fn remove_user_messenger(app: AppHandle, id: String) -> Result<(), String> {
    let label = custom_webview_label(&id);
    if let Some(webview) = app.get_webview(&label) {
        webview.close().map_err(|e| e.to_string())?;
    }
    app.state::<UserMessengers>().0.lock().unwrap().retain(|m| m.id != id);
    persist_user_messengers(&app)?;
    update_tray(&app);
    Ok(())
}

#[tauri::command]
async fn open_add_messenger_window(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("add-messenger") {
        let _ = win.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(
        &app,
        "add-messenger",
        WebviewUrl::App("index.html?view=add-messenger".into()),
    )
    .title("Add Messenger")
    .inner_size(480.0, 640.0)
    .min_inner_size(480.0, 420.0)
    .resizable(true)
    .devtools(cfg!(debug_assertions))
    .center()
    .build()
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn get_recent_logs(app: AppHandle, lines: u32) -> Result<String, String> {
    let log_dir = app.path().app_log_dir().map_err(|e| e.to_string())?;
    let log_path = log_dir.join("signalist.log");
    if !log_path.exists() {
        return Ok("No log file found yet.".to_string());
    }
    let content = std::fs::read_to_string(&log_path).map_err(|e| e.to_string())?;
    let collected: Vec<&str> = content.lines().collect();
    let start = collected.len().saturating_sub(lines as usize);
    Ok(collected[start..].join("\n"))
}

#[tauri::command]
fn log_js_error(source: String, message: String, stack: String) {
    log::error!("[JS:{}] {} | stack: {}", source, message, stack);
}

#[tauri::command]
async fn open_bug_report_window(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("bug-report") {
        let _ = win.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(
        &app,
        "bug-report",
        WebviewUrl::App("index.html?view=bug-report".into()),
    )
    .title("Bug Report")
    .inner_size(560.0, 420.0)
    .min_inner_size(400.0, 320.0)
    .resizable(true)
    .devtools(cfg!(debug_assertions))
    .center()
    .build()
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn open_custom_shortcut(
    app: AppHandle,
    id: String,
    url: String,
) -> Result<String, String> {
    ensure_custom_webview(app, id, url, true).await
}

/// Shared by shortcuts and user messengers. With `activate == false` the webview
/// is created off-screen for startup preloading: the session stays warm and
/// switching to it later is instant, but it never steals the visible slot.
async fn ensure_custom_webview(
    app: AppHandle,
    id: String,
    url: String,
    activate: bool,
) -> Result<String, String> {
    let label = custom_webview_label(&id);

    if let Some(webview) = app.get_webview(&label) {
        if !activate {
            return Ok(format!("{} already loaded", label));
        }
        hide_all_messengers(&app);
        // Custom webviews are shrunk to 0×0 while hidden — restore before showing.
        let window = app.get_window("main").ok_or("Main window not found")?;
        let (pos, size) = content_bounds(get_logical_size(&window)?);
        let _ = webview.set_position(pos);
        let _ = webview.set_size(size);
        webview.show().map_err(|e| e.to_string())?;
        webview.set_focus().map_err(|e| e.to_string())?;
        *app.state::<ActiveMessenger>().0.lock().unwrap() = label.clone();
        let _ = app.emit("active-messenger-changed", label.clone());
        return Ok(format!("Focused existing {}", label));
    }

    let parsed_url: tauri::Url = url.parse().map_err(|e| format!("Invalid URL: {}", e))?;
    let origin_host = parsed_url.host_str().ok_or("URL has no host")?.to_string();
    if is_google_domain(&origin_host) {
        return Err("Google services are not supported in the embedded window. Please delete this shortcut and open the website in your browser.".into());
    }
    let data_store_id = shortcut_id_to_data_store_id(&id);

    let nav_guard = move |nav_url: &tauri::Url| -> bool {
        matches!(nav_url.scheme(), "https" | "http")
    };

    let window = app.get_window("main").ok_or("Main window not found")?;
    let logical = get_logical_size(&window)?;

    let inject = include_str!("../inject/shortcut.js");

    let webview_builder = WebviewBuilder::new(&label, WebviewUrl::External(parsed_url))
        .user_agent(SAFARI_UA)
        .data_store_identifier(data_store_id)
        .on_navigation(nav_guard)
        .devtools(cfg!(debug_assertions))
        // Same as messengers: allow native HTML5 file drops into the web app.
        .disable_drag_drop_handler()
        .initialization_script(inject);

    // Preloaded shortcuts keep the default (suspend) throttling policy: they
    // don't track anything in the background, so letting WebKit park them after
    // a few minutes is exactly what we want — the session stays signed in and
    // wakes up on show.
    let webview_builder = if activate { webview_builder } else { webview_builder.focused(false) };

    if activate {
        hide_all_messengers(&app);
    }
    let child = window
        .add_child(
            webview_builder,
            LogicalPosition::new(SIDEBAR_WIDTH, 0.0),
            LogicalSize::new(logical.width - SIDEBAR_WIDTH, logical.height),
        )
        .map_err(|e| e.to_string())?;

    if !activate {
        // Preloaded shortcuts follow the hidden-state convention of
        // hide_all_messengers: 0×0 so the compositor releases the IOSurface.
        let _ = child.hide();
        let _ = child.set_size(LogicalSize::new(0.0, 0.0));
        return Ok(format!("Preloaded {}", label));
    }

    *app.state::<ActiveMessenger>().0.lock().unwrap() = label.clone();
    let _ = app.emit("active-messenger-changed", label.clone());
    Ok(format!("Created {}", label))
}

// On macOS, a hidden WKWebView keeps its IOSurface backing store allocated for
// fast re-show — fine for one or two views, but with N custom shortcuts the
// cumulative GPU pressure can starve WindowServer. For shortcuts/user messengers
// (which don't run background tracking — their inject is shortcut.js, no unread
// reporting), shrinking to 0×0 forces the compositor to drop the surface.
// MESSENGERS (Telegram/WhatsApp) keep full size while hidden so their inject
// scripts continue polling unread counts in the background.
fn hide_all_messengers(app: &AppHandle) {
    for m in MESSENGERS {
        if let Some(webview) = app.get_webview(m.label) {
            let _ = webview.hide();
        }
    }
    let zero = LogicalSize::new(0.0, 0.0);
    if let Some(state) = app.try_state::<UserMessengers>() {
        let labels: Vec<String> = state.0.lock().unwrap().iter().map(|m| m.webview_label()).collect();
        for label in &labels {
            if let Some(webview) = app.get_webview(label) {
                let _ = webview.hide();
                let _ = webview.set_size(zero);
            }
        }
    }
    if let Some(state) = app.try_state::<CustomShortcuts>() {
        let labels: Vec<String> = state.0.lock().unwrap().iter().map(|sc| sc.webview_label()).collect();
        for label in &labels {
            if let Some(webview) = app.get_webview(label) {
                let _ = webview.hide();
                let _ = webview.set_size(zero);
            }
        }
    }
}

fn do_show_window(app: &AppHandle) {
    let Some(window) = app.get_window("main") else { return };
    let _ = window.show();
    let _ = window.set_focus();
    let active = app.state::<ActiveMessenger>().0.lock().unwrap().clone();
    if !active.is_empty() {
        if let Some(webview) = app.get_webview(&active) {
            let _ = webview.show();
            let _ = webview.set_focus();
        }
    }
}

#[tauri::command]
fn show_window(app: AppHandle) {
    do_show_window(&app);
}

fn toggle_window(app: &AppHandle) {
    let Some(window) = app.get_window("main") else { return };
    if window.is_fullscreen().unwrap_or(false) {
        if !window.is_focused().unwrap_or(false) {
            let _ = window.set_focus();
        }
        return;
    }
    let visible = window.is_visible().unwrap_or(false);
    let focused = window.is_focused().unwrap_or(false);
    if visible && focused {
        let _ = window.hide();
    } else if visible {
        let _ = window.set_focus();
    } else {
        do_show_window(app);
    }
}

#[tauri::command]
fn toggle_dock_icon(app: AppHandle) {
    do_toggle_dock_icon(&app);
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(app: AppHandle, enable: bool) -> Result<(), String> {
    let autolaunch = app.autolaunch();
    if enable {
        autolaunch.enable().map_err(|e| e.to_string())
    } else {
        autolaunch.disable().map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn get_silence_mode(state: State<SilenceMode>) -> bool {
    *state.0.lock().unwrap()
}

#[tauri::command]
fn set_silence_mode(app: AppHandle, state: State<SilenceMode>, enable: bool) -> Result<(), String> {
    {
        let mut v = state.0.lock().unwrap();
        if *v == enable { return Ok(()); }
        *v = enable;
    }
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    store.set("silence_mode", enable);
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn get_global_shortcut(state: State<HotkeyConfig>) -> String {
    state.0.lock().unwrap().clone()
}

#[tauri::command]
fn set_global_shortcut(
    app: AppHandle,
    state: State<HotkeyConfig>,
    shortcut: String,
) -> Result<(), String> {
    let old = state.0.lock().unwrap().clone();
    if !old.is_empty() {
        let _ = app.global_shortcut().unregister(old.as_str());
    }
    app.global_shortcut()
        .on_shortcut(shortcut.as_str(), |handle, _, event| {
            if event.state() == ShortcutState::Pressed {
                toggle_window(handle);
            }
        })
        .map_err(|e| e.to_string())?;
    *state.0.lock().unwrap() = shortcut.clone();
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    store.set("hotkey", serde_json::Value::String(shortcut));
    store.save().map_err(|e| e.to_string())?;
    update_tray(&app);
    Ok(())
}

// Gap between two preloaded webviews. Each one spawns a WebContent process and
// pulls a full web app over the network; starting them back-to-back makes the
// first seconds after launch noticeably janky, so they are staggered.
const PRELOAD_STAGGER_MS: u64 = 600;

/// Creates the startup set of webviews on a background thread: the visible one
/// first, then every messenger flagged for preloading, hidden.
///
/// Preloading is what makes background notifications work at all — a messenger
/// that was never opened has no webview, so its inject script never runs and
/// never calls `update_unread_count`.
fn spawn_startup_preload(app: AppHandle) {
    std::thread::spawn(move || {
        let builtins: Vec<String> = MESSENGERS
            .iter()
            .filter(|m| builtin_preload_enabled(&app, m.label))
            .map(|m| m.label.to_string())
            .collect();

        let user_targets: Vec<(String, String)> = app
            .try_state::<UserMessengers>()
            .map(|state| {
                state.0.lock().unwrap()
                    .iter()
                    .filter(|m| m.preload)
                    .map(|m| (m.id.clone(), m.url.clone()))
                    .collect()
            })
            .unwrap_or_default();

        // The first preloaded built-in takes the visible slot. With every flag
        // off we still open Telegram, so the content area is never blank.
        let active = builtins
            .first()
            .cloned()
            .unwrap_or_else(|| MESSENGERS[0].label.to_string());

        if let Err(e) = tauri::async_runtime::block_on(ensure_messenger_webview(
            app.clone(),
            active.clone(),
            true,
        )) {
            log::error!("Failed to open {} at startup: {}", active, e);
        }

        for label in builtins.into_iter().filter(|l| *l != active) {
            std::thread::sleep(Duration::from_millis(PRELOAD_STAGGER_MS));
            match tauri::async_runtime::block_on(ensure_messenger_webview(
                app.clone(),
                label.clone(),
                false,
            )) {
                Ok(_) => log::info!("Preloaded messenger {} in background", label),
                Err(e) => log::warn!("Failed to preload {}: {}", label, e),
            }
        }

        for (id, url) in user_targets {
            std::thread::sleep(Duration::from_millis(PRELOAD_STAGGER_MS));
            match tauri::async_runtime::block_on(ensure_custom_webview(
                app.clone(),
                id.clone(),
                url,
                false,
            )) {
                Ok(_) => log::info!("Preloaded user messenger {} in background", id),
                Err(e) => log::warn!("Failed to preload messenger {}: {}", id, e),
            }
        }
    });
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let msg = info.to_string();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = format!("[PANIC {}] {}\n", ts, msg);
        eprintln!("{}", line.trim());
        if let Ok(home) = std::env::var("HOME") {
            let dir = std::path::PathBuf::from(home).join("Library/Logs/com.signalist.app");
            let _ = std::fs::create_dir_all(&dir);
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("crash.log"))
            {
                let _ = f.write_all(line.as_bytes());
            }
        }
    }));
}

// Signalist relies on BackgroundThrottlingPolicy::Disabled (macOS 14+) for background
// unread tracking; on older systems messengers would silently stop reporting unread
// counts after ~5 minutes. Better to refuse to start with a clear message than to run
// in a degraded state the user can't diagnose.
const MIN_MACOS_MAJOR_VERSION: u32 = 14;

#[cfg(target_os = "macos")]
fn check_macos_version_or_exit() {
    let output = match std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
    {
        Ok(out) if out.status.success() => out,
        _ => return, // can't determine version — don't block startup on a detection failure
    };

    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let major: u32 = match version.split('.').next().and_then(|s| s.parse().ok()) {
        Some(m) => m,
        None => return,
    };

    if major == 0 || major >= MIN_MACOS_MAJOR_VERSION {
        return;
    }

    let lines = [
        "Signalist потребує macOS 14 Sonoma або новіше.".to_string(),
        format!("Ваша версія: macOS {}", version),
        "Оновіть систему через System Settings → General → Software Update, щоб застосунок працював коректно.".to_string(),
    ];
    let quoted = lines
        .iter()
        .map(|l| format!("\"{}\"", l.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(" & return & return & ");
    let script = format!(
        "display dialog {} buttons {{\"OK\"}} default button \"OK\" with icon caution with title \"Signalist\"",
        quoted
    );

    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .status();

    std::process::exit(1);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "macos")]
    check_macos_version_or_exit();
    install_panic_hook();
    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stderr),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                        file_name: Some("signalist".to_string()),
                    }),
                ])
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepOne)
                .max_file_size(5_000_000)
                .level(log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(ActiveMessenger(Mutex::new(String::new())))
        .manage(UnreadCounts::default())
        .manage(NotifyTracker::default())
        .manage(HotkeyConfig(Mutex::new(String::new())))
        .manage(DockHidden(Mutex::new(false)))
        .manage(SilenceMode(Mutex::new(false)))
        .manage(CustomShortcuts(Mutex::new(Vec::new())))
        .manage(UserMessengers(Mutex::new(Vec::new())))
        .manage(BuiltinPreload::default())
        .invoke_handler(tauri::generate_handler![
            open_messenger,
            switch_messenger,
            close_messenger,
            get_active_messenger,
            update_sidebar_theme_from_webview,
            update_unread_count,
            get_silence_mode,
            set_silence_mode,
            get_global_shortcut,
            set_global_shortcut,
            get_autostart,
            set_autostart,
            toggle_dock_icon,
            open_add_shortcut_window,
            open_edit_shortcut_window,
            open_confirm_delete_window,
            list_custom_shortcuts,
            add_custom_shortcut,
            update_custom_shortcut,
            remove_custom_shortcut,
            reorder_custom_shortcuts,
            open_custom_shortcut,
            list_user_messengers,
            add_user_messenger,
            remove_user_messenger,
            get_builtin_preload,
            set_builtin_preload,
            set_user_messenger_preload,
            open_add_messenger_window,
            get_recent_logs,
            log_js_error,
            open_bug_report_window,
            open_in_browser,
            show_window,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            let resize_handle = app.handle().clone();

            let window = WindowBuilder::new(app, "main")
                .title("Signalist")
                .inner_size(1200.0, 800.0)
                .min_inner_size(800.0, 600.0)
                .resizable(true)
                .decorations(true)
                .transparent(true)
                .build()?;

            let logical = get_logical_size(&window)?;

            let sidebar_builder =
                WebviewBuilder::new("sidebar", WebviewUrl::App("index.html".into()))
                    .transparent(true)
                    .devtools(cfg!(debug_assertions));

            window.add_child(
                sidebar_builder,
                LogicalPosition::new(0.0, 0.0),
                LogicalSize::new(SIDEBAR_WIDTH, logical.height),
            )?;

            #[cfg(target_os = "macos")]
            if let Err(e) = apply_vibrancy(&window, NSVisualEffectMaterial::Sidebar, None, None) {
                log::warn!("Vibrancy unavailable: {}", e);
            }

            window.on_window_event(move |event| {
                if let WindowEvent::Resized(_) = event {
                    reposition_webviews(&resize_handle);
                }
            });

            let store = app.handle().store("settings.json")
                .map_err(|e| { log::error!("Failed to open settings store: {}", e); e })
                .expect("Failed to open settings store");
            let saved_hotkey = store
                .get("hotkey")
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "Super+Shift+S".to_string());
            *app.state::<HotkeyConfig>().0.lock().unwrap() = saved_hotkey.clone();

            let saved_shortcuts: Vec<CustomShortcut> = store
                .get("custom_shortcuts")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            *app.state::<CustomShortcuts>().0.lock().unwrap() = saved_shortcuts;

            let saved_silence = store.get("silence_mode").and_then(|v| v.as_bool()).unwrap_or(false);
            *app.state::<SilenceMode>().0.lock().unwrap() = saved_silence;

            let saved_user_messengers: Vec<UserMessenger> = store
                .get("user_messengers")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            *app.state::<UserMessengers>().0.lock().unwrap() = saved_user_messengers;

            let saved_builtin_preload: HashMap<String, bool> = store
                .get("builtin_preload")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            *app.state::<BuiltinPreload>().0.lock().unwrap() = saved_builtin_preload;

            // After the store is loaded, so the preload set reflects saved flags.
            spawn_startup_preload(handle.clone());

            app.handle()
                .global_shortcut()
                .on_shortcut(saved_hotkey.as_str(), |app_handle, _, event| {
                    if event.state() == ShortcutState::Pressed {
                        toggle_window(app_handle);
                    }
                })
                .unwrap_or_else(|e| { log::error!("Failed to register global shortcut '{}': {}", saved_hotkey, e); });

            // Build tray icon
            let Some(tray_menu) = build_tray_menu(app.handle()) else {
                log::error!("Failed to build tray menu");
                return Ok(());
            };
            let Some(icon) = app.default_window_icon().cloned() else {
                log::error!("No default window icon found");
                return Ok(());
            };

            TrayIconBuilder::with_id("main-tray")
                .icon(icon)
                .icon_as_template(true)
                .menu(&tray_menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| {
                    match event.id.as_ref() {
                        id @ ("telegram" | "whatsapp") => {
                            if let Some(window) = app.get_window("main") {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                            let messenger = id.to_string();
                            let app_clone = app.clone();
                            tauri::async_runtime::spawn(async move {
                                let _ = open_messenger(app_clone, messenger).await;
                            });
                        }
                        id if id.starts_with("custom-") => {
                            if let Some(window) = app.get_window("main") {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                            let id_str = id.to_string();
                            let shortcut_match = app.state::<CustomShortcuts>().0.lock().unwrap()
                                .iter()
                                .find(|s| s.webview_label() == id_str)
                                .map(|sc| (sc.id.clone(), sc.url.clone()));
                            let user_match = app.try_state::<UserMessengers>().and_then(|state| {
                                state.0.lock().unwrap()
                                    .iter()
                                    .find(|m| m.webview_label() == id_str)
                                    .map(|m| (m.id.clone(), m.url.clone()))
                            });
                            if let Some((entry_id, entry_url)) = shortcut_match.or(user_match) {
                                let app_clone = app.clone();
                                tauri::async_runtime::spawn(async move {
                                    let _ = open_custom_shortcut(app_clone, entry_id, entry_url).await;
                                });
                            }
                        }
                        "toggle_window" => toggle_window(app),
                        "toggle_dock" => do_toggle_dock_icon(app),
                        "quit" => std::process::exit(0),
                        _ => {}
                    }
                })
                .build(app)?;

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app_handle, event| {
            if let RunEvent::ExitRequested { api, .. } = event {
                api.prevent_exit();
            }
        });
}
