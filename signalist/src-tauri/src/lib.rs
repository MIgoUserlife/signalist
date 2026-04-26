use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use serde::Serialize;
use tauri::{
    path::BaseDirectory,
    webview::WebviewBuilder, AppHandle, Emitter, LogicalPosition, LogicalSize, Manager,
    RunEvent, WebviewUrl, WindowBuilder, WindowEvent,
};
use tauri_plugin_notification::NotificationExt;

const SIDEBAR_WIDTH: f64 = 72.0;

const CHROME_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

struct Messenger {
    label: &'static str,
    url: &'static str,
    allowed_domains: &'static [&'static str],
    data_store_id: [u8; 16],
}

const MESSENGERS: &[Messenger] = &[
    Messenger {
        label: "telegram",
        url: "https://web.telegram.org/a/",
        allowed_domains: &["web.telegram.org", "t.me"],
        data_store_id: [
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x01,
        ],
    },
    Messenger {
        label: "whatsapp",
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

#[derive(Default)]
pub struct LastNotified {
    pub counts: Mutex<HashMap<String, u32>>,
    pub timestamps: Mutex<HashMap<String, Instant>>,
}

#[derive(Clone, Serialize)]
struct UnreadUpdatePayload {
    messenger: String,
    count: u32,
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

    if let Some(state) = app.try_state::<UnreadCounts>() {
        let mut map = state.0.lock().unwrap();
        if map.get(&messenger).copied() == Some(count) {
            return;
        }
        map.insert(messenger.clone(), count);
    }

    let _ = app.emit("unread-update", UnreadUpdatePayload { messenger: messenger.clone(), count });

    // When count drops to 0, reset LastNotified baseline so future increases notify
    if count == 0 {
        if let Some(ln) = app.try_state::<LastNotified>() {
            let mut counts = ln.counts.lock().unwrap();
            counts.insert(messenger.clone(), 0);
        }
        return;
    }

    if let Some(ln) = app.try_state::<LastNotified>() {
        let mut counts = ln.counts.lock().unwrap();
        let mut timestamps = ln.timestamps.lock().unwrap();
        let last_notified = counts.get(&messenger).copied().unwrap_or(0);
        let cooldown_ok = match timestamps.get(&messenger) {
            Some(last) => last.elapsed() >= Duration::from_secs(5),
            None => true,
        };

        if count > last_notified && cooldown_ok {
            let display_name = match messenger.as_str() {
                "telegram" => "Telegram",
                "whatsapp" => "WhatsApp",
                _ => &messenger,
            };
            let body = if count == 1 {
                "You have 1 unread message".to_string()
            } else {
                format!("You have {} unread messages", count)
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
                Ok(()) => println!("[Signalist] Notification sent for {}", display_name),
                #[cfg(not(debug_assertions))]
                Ok(()) => {}
                Err(e) => eprintln!("[Signalist] Notification failed for {}: {}", display_name, e),
            }

            counts.insert(messenger.clone(), count);
            timestamps.insert(messenger.clone(), Instant::now());
        }
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

    let content_width = logical.width - SIDEBAR_WIDTH;
    for m in MESSENGERS {
        if let Some(webview) = app.get_webview(m.label) {
            let _ = webview.set_position(LogicalPosition::new(SIDEBAR_WIDTH, 0.0));
            let _ = webview.set_size(LogicalSize::new(content_width, logical.height));
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

    Ok(format!("Created {}", config.label))
}

#[tauri::command]
fn switch_messenger(app: AppHandle, messenger: String) -> Result<(), String> {
    let config = MESSENGERS
        .iter()
        .find(|m| m.label == messenger)
        .ok_or_else(|| format!("Unknown messenger: {}", messenger))?;

    if app.get_webview(config.label).is_none() {
        return Err(format!(
            "{} webview not created yet. Call open_messenger first.",
            messenger
        ));
    }

    hide_all_messengers(&app);

    let webview = app.get_webview(config.label).unwrap();
    webview.show().map_err(|e| e.to_string())?;
    webview.set_focus().map_err(|e| e.to_string())?;

    let state = app.state::<ActiveMessenger>();
    let mut active = state.0.lock().unwrap();
    *active = messenger;

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

fn hide_all_messengers(app: &AppHandle) {
    for m in MESSENGERS {
        if let Some(webview) = app.get_webview(m.label) {
            let _ = webview.hide();
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .manage(ActiveMessenger(Mutex::new(String::new())))
        .manage(UnreadCounts::default())
        .manage(LastNotified::default())
        .invoke_handler(tauri::generate_handler![
            open_messenger,
            switch_messenger,
            close_messenger,
            get_active_messenger,
            update_unread_count,
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
                .build()?;

            let logical = get_logical_size(&window)?;

            let sidebar_builder =
                WebviewBuilder::new("sidebar", WebviewUrl::App("index.html".into()));

            window.add_child(
                sidebar_builder,
                LogicalPosition::new(0.0, 0.0),
                LogicalSize::new(SIDEBAR_WIDTH, logical.height),
            )?;

            let open_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                let _ = open_messenger(open_handle, "telegram".into()).await;
            });

            window.on_window_event(move |event| {
                if let WindowEvent::Resized(_) = event {
                    reposition_webviews(&resize_handle);
                }
            });

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
