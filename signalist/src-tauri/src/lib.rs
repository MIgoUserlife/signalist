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
        allowed_domains: &["web.whatsapp.com", "whatsapp.com", "static.whatsapp.net"],
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomShortcut {
    pub id: String,
    pub name: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
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
}

impl UserMessenger {
    fn webview_label(&self) -> String {
        custom_webview_label(&self.id)
    }
}

pub struct UserMessengers(pub Mutex<Vec<UserMessenger>>);

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

fn domain_to_data_store_id(domain: &str) -> [u8; 16] {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in domain.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let h = hash.to_le_bytes();
    [0xCC, h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 0, 0, 0, 0, 0, 0, 0]
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

fn build_tray_menu(app: &AppHandle) -> Menu<tauri::Wry> {
    let counts = app.state::<UnreadCounts>().0.lock().unwrap().clone();
    let hotkey = app.state::<HotkeyConfig>().0.lock().unwrap().clone();
    let dock_hidden = *app.state::<DockHidden>().0.lock().unwrap();

    let menu = Menu::new(app).unwrap();

    for m in MESSENGERS {
        let count = counts.get(m.label).copied().unwrap_or(0);
        let dot = if count > 0 { "◉" } else { "○" };
        let label = format!("{}  {}", dot, m.display_name);
        let item = MenuItem::with_id(app, m.label, &label, true, None::<&str>).unwrap();
        menu.append(&item).unwrap();
    }

    if let Some(state) = app.try_state::<UserMessengers>() {
        let entries: Vec<(String, String)> = state.0.lock().unwrap()
            .iter()
            .map(|m| (m.webview_label(), m.name.clone()))
            .collect();
        for (webview_label, name) in &entries {
            let label = format!("○  {}", name);
            let item = MenuItem::with_id(app, webview_label, &label, true, None::<&str>).unwrap();
            menu.append(&item).unwrap();
        }
    }

    if let Some(state) = app.try_state::<CustomShortcuts>() {
        let shortcuts: Vec<(String, String)> = state.0.lock().unwrap()
            .iter()
            .map(|sc| (sc.webview_label(), sc.name.clone()))
            .collect();
        for (webview_label, name) in &shortcuts {
            let label = format!("○  {}", name);
            let item = MenuItem::with_id(app, webview_label, &label, true, None::<&str>).unwrap();
            menu.append(&item).unwrap();
        }
    }

    menu.append(&PredefinedMenuItem::separator(app).unwrap()).unwrap();

    let accel = hotkey.replace("Super", "Cmd");
    let toggle_item = MenuItem::with_id(
        app,
        "toggle_window",
        "⧉  Show/Hide",
        true,
        Some(accel.as_str()),
    )
    .unwrap();
    menu.append(&toggle_item).unwrap();

    menu.append(&PredefinedMenuItem::separator(app).unwrap()).unwrap();

    let dock_label = if dock_hidden {
        "▭  Show in Dock"
    } else {
        "▭  Hide in Dock"
    };
    let dock_item = MenuItem::with_id(app, "toggle_dock", dock_label, true, None::<&str>).unwrap();
    menu.append(&dock_item).unwrap();

    menu.append(&PredefinedMenuItem::separator(app).unwrap()).unwrap();

    let quit_item = MenuItem::with_id(app, "quit", "⏻  Quit", true, Some("Cmd+Q")).unwrap();
    menu.append(&quit_item).unwrap();

    menu
}

fn update_tray(app: &AppHandle) {
    let Some(tray) = app.tray_by_id("main-tray") else { return };
    let menu = build_tray_menu(app);
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
fn update_unread_count(app: AppHandle, messenger: String, count: u32) {
    if !MESSENGERS.iter().any(|m| m.label == messenger.as_str()) {
        eprintln!("[Signalist] update_unread_count REJECTED unknown messenger: {}", messenger);
        return;
    }
    let count = count.min(10_000);
    #[cfg(debug_assertions)]
    println!("[Signalist] update_unread_count CALLED for {} with count {}", messenger, count);

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

    {
        let Some(tracker) = app.try_state::<NotifyTracker>() else { return };
        let mut map = tracker.0.lock().unwrap();
        let entry = map.entry(messenger.to_string()).or_default();
        if current_count <= entry.last_notified {
            return;
        }
        entry.last_notified = current_count;
    }

    // Baseline is now updated. If the user is looking at the app, suppress the
    // OS notification — they can already see the count in the sidebar.
    let window_focused = app
        .get_window("main")
        .and_then(|w| w.is_focused().ok())
        .unwrap_or(false);
    if window_focused {
        return;
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
        #[cfg(debug_assertions)]
        Ok(()) => println!(
            "[Signalist] Notification sent for {} (count={})",
            display_name, current_count
        ),
        #[cfg(not(debug_assertions))]
        Ok(()) => {}
        Err(e) => eprintln!(
            "[Signalist] Notification failed for {}: {}",
            display_name, e
        ),
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
    let config = MESSENGERS
        .iter()
        .find(|m| m.label == messenger)
        .ok_or_else(|| format!("Unknown messenger: {}", messenger))?;

    if let Some(webview) = app.get_webview(config.label) {
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
            allowed_domains
                .iter()
                .any(|d| host == *d || host.ends_with(&format!(".{}", d)))
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
        .initialization_script(init_script);

    hide_all_messengers(&app);

    window
        .add_child(
            webview_builder,
            LogicalPosition::new(SIDEBAR_WIDTH, 0.0),
            LogicalSize::new(content_width, content_height),
        )
        .map_err(|e| e.to_string())?;

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
    .inner_size(360.0, 480.0)
    .min_inner_size(360.0, 480.0)
    .resizable(false)
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
    .inner_size(360.0, 400.0)
    .min_inner_size(360.0, 400.0)
    .resizable(false)
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
) -> Result<CustomShortcut, String> {
    let parsed: tauri::Url = url.parse().map_err(|e| format!("Invalid URL: {}", e))?;
    let host = parsed.host_str().ok_or("URL has no host")?;
    if is_google_domain(host) {
        return Err("Google services (Gemini, Google, YouTube) are not supported in the embedded window due to Google's policy".into());
    }

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
fn add_custom_shortcut(app: AppHandle, name: String, url: String, icon: Option<String>) -> Result<CustomShortcut, String> {
    let parsed: tauri::Url = url.parse().map_err(|e| format!("Invalid URL: {}", e))?;
    let host = parsed.host_str().ok_or("URL has no host")?;
    if is_google_domain(host) {
        return Err("Google services (Gemini, Google, YouTube) are not supported in the embedded window due to Google's policy".into());
    }
    let sc = CustomShortcut { id: generate_shortcut_id(), name, url, icon };
    app.state::<CustomShortcuts>().0.lock().unwrap().push(sc.clone());
    persist_custom_shortcuts(&app)?;
    update_tray(&app);
    Ok(sc)
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
) -> Result<UserMessenger, String> {
    let parsed: tauri::Url = url.parse().map_err(|e| format!("Invalid URL: {}", e))?;
    let host = parsed.host_str().ok_or("URL has no host")?;
    if is_google_domain(host) {
        return Err("Google services are not supported in the embedded window".into());
    }
    let m = UserMessenger { id: generate_shortcut_id(), name, url, icon };
    app.state::<UserMessengers>().0.lock().unwrap().push(m.clone());
    persist_user_messengers(&app)?;
    update_tray(&app);
    Ok(m)
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
    .inner_size(480.0, 360.0)
    .min_inner_size(480.0, 360.0)
    .resizable(false)
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
    let label = custom_webview_label(&id);

    if let Some(webview) = app.get_webview(&label) {
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
    let data_store_id = domain_to_data_store_id(&origin_host);

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
        .initialization_script(inject);

    hide_all_messengers(&app);
    window
        .add_child(
            webview_builder,
            LogicalPosition::new(SIDEBAR_WIDTH, 0.0),
            LogicalSize::new(logical.width - SIDEBAR_WIDTH, logical.height),
        )
        .map_err(|e| e.to_string())?;

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
        .manage(CustomShortcuts(Mutex::new(Vec::new())))
        .manage(UserMessengers(Mutex::new(Vec::new())))
        .invoke_handler(tauri::generate_handler![
            open_messenger,
            switch_messenger,
            close_messenger,
            get_active_messenger,
            update_unread_count,
            get_global_shortcut,
            set_global_shortcut,
            get_autostart,
            set_autostart,
            toggle_dock_icon,
            open_add_shortcut_window,
            open_edit_shortcut_window,
            list_custom_shortcuts,
            add_custom_shortcut,
            update_custom_shortcut,
            remove_custom_shortcut,
            open_custom_shortcut,
            list_user_messengers,
            add_user_messenger,
            remove_user_messenger,
            open_add_messenger_window,
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
                    .transparent(true);

            window.add_child(
                sidebar_builder,
                LogicalPosition::new(0.0, 0.0),
                LogicalSize::new(SIDEBAR_WIDTH, logical.height),
            )?;

            #[cfg(target_os = "macos")]
            apply_vibrancy(&window, NSVisualEffectMaterial::Sidebar, None, None)
                .expect("Failed to apply vibrancy");

            let open_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                let _ = open_messenger(open_handle, "telegram".into()).await;
            });

            window.on_window_event(move |event| {
                if let WindowEvent::Resized(_) = event {
                    reposition_webviews(&resize_handle);
                }
            });

            let store = app.handle().store("settings.json")
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

            let saved_user_messengers: Vec<UserMessenger> = store
                .get("user_messengers")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            *app.state::<UserMessengers>().0.lock().unwrap() = saved_user_messengers;

            app.handle()
                .global_shortcut()
                .on_shortcut(saved_hotkey.as_str(), |app_handle, _, event| {
                    if event.state() == ShortcutState::Pressed {
                        toggle_window(app_handle);
                    }
                })
                .expect("Failed to register global shortcut");

            // Build tray icon
            let tray_menu = build_tray_menu(app.handle());
            let icon = app.default_window_icon().cloned()
                .expect("No default window icon");

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
