//! In-app hotkeys: a configurable "super" modifier plus one key per action,
//! delivered through the native macOS menu bar.
//!
//! Why the menu and not a `keydown` listener: while a messenger is on screen the
//! keyboard focus sits in its third-party webview, so the sidebar never sees the
//! keystroke. A menu key equivalent is resolved by AppKit before the event
//! reaches any webview, and only while Signalist is the active app — unlike a
//! global shortcut, it never steals the combination from other applications.
//! The one global binding (show/hide the window) stays in `HotkeyConfig`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Emitter, EventTarget, Manager, State, WindowEvent, Wry};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};
use tauri_plugin_store::{Store, StoreExt};

use crate::{
    apply_silence_mode, dialog_window, ensure_custom_webview, ensure_messenger_webview, is_safe_shortcut_id,
    open_add_messenger_window, open_add_shortcut_window, open_edit_shortcut_window,
    register_toggle_shortcut, ActiveMessenger, CustomShortcuts, HotkeyConfig, ShortcutId,
    SilenceMode, StoredId, UnreadCounts, UserMessengers, MESSENGERS, SETTINGS_STORE,
};

const KEYMAP_STORE_KEY: &str = "keymap";

/// Prefix of every menu item id this module owns. Menu events are app-wide —
/// the tray's items arrive at the same handler — so the prefix is what keeps a
/// tray id such as `telegram` from being read as a hotkey action.
const MENU_ID_PREFIX: &str = "hk:";

/// Canonical modifier names, in the order macOS prints them (⌃⌥⌘). Both muda
/// (menu accelerators) and global-hotkey parse these spellings.
///
/// ⇧ is deliberately not a super modifier. muda sets the key equivalent to the
/// unshifted character (`1`, `[`) plus a Shift mask, while AppKit matches the
/// character the keystroke produces (`!`, `{`), so every digit and punctuation
/// binding would silently never fire — only letters would.
const MODIFIER_ORDER: &[&str] = &["Ctrl", "Alt", "Cmd"];

const DEFAULT_MODIFIERS: &[&str] = &["Ctrl", "Alt"];

/// Keys an action may be bound to: exactly the set both accelerator parsers
/// accept as a plain character, so a stored key always builds a menu item.
const ALLOWED_KEYS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789[],./;'-=`\\";

/// Positions `1…9` in the sidebar order. Positions past nine get no hotkey.
const GOTO_SLOTS: usize = 9;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyAction {
    id: &'static str,
    label: &'static str,
    default_key: &'static str,
}

const fn action(id: &'static str, label: &'static str, default_key: &'static str) -> HotkeyAction {
    HotkeyAction { id, label, default_key }
}

/// Every action, in the order the editor, the cheatsheet and the menu list them.
/// The `goto-N` ids must stay contiguous from `goto-1`: `goto_slot` parses them.
pub const ACTIONS: &[HotkeyAction] = &[
    action("goto-1", "Go to item 1", "1"),
    action("goto-2", "Go to item 2", "2"),
    action("goto-3", "Go to item 3", "3"),
    action("goto-4", "Go to item 4", "4"),
    action("goto-5", "Go to item 5", "5"),
    action("goto-6", "Go to item 6", "6"),
    action("goto-7", "Go to item 7", "7"),
    action("goto-8", "Go to item 8", "8"),
    action("goto-9", "Go to item 9", "9"),
    action("next", "Next item", "]"),
    action("prev", "Previous item", "["),
    action("first-unread", "First messenger with unread", "0"),
    action("add-shortcut", "Add shortcut…", "N"),
    action("add-messenger", "Add messenger…", "M"),
    action("edit-shortcut", "Edit current shortcut…", "E"),
    action("reload", "Reload current page", "R"),
    action("toggle-silence", "Toggle silence mode", "D"),
    action("hotkeys", "Hotkey settings…", ","),
    action("cheatsheet", "Hotkey cheatsheet", "/"),
];

fn goto_slot(action_id: &str) -> Option<usize> {
    let n: usize = action_id.strip_prefix("goto-")?.parse().ok()?;
    (1..=GOTO_SLOTS).contains(&n).then_some(n - 1)
}

/// The persisted keymap. An empty key string means the action is unbound.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Keymap {
    pub modifiers: Vec<String>,
    pub keys: HashMap<String, String>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            modifiers: DEFAULT_MODIFIERS.iter().map(|m| m.to_string()).collect(),
            keys: ACTIONS
                .iter()
                .map(|a| (a.id.to_string(), a.default_key.to_string()))
                .collect(),
        }
    }
}

impl Keymap {
    /// Canonicalizes and checks a keymap. Unknown actions are dropped and
    /// missing ones take their default — unless that default is already taken,
    /// in which case the action stays unbound rather than failing the whole map
    /// (this is what lets a future release add an action to an old keymap).
    fn normalized(self) -> Result<Self, String> {
        let mut modifiers = Vec::new();
        for m in &self.modifiers {
            if m.eq_ignore_ascii_case("Shift") {
                return Err("⇧ can't be part of the super key: macOS would ignore the digit and punctuation hotkeys".into());
            }
            let canonical = MODIFIER_ORDER
                .iter()
                .find(|c| c.eq_ignore_ascii_case(m))
                .ok_or_else(|| format!("Unknown modifier: {}", m))?;
            if !modifiers.contains(canonical) {
                modifiers.push(*canonical);
            }
        }
        modifiers.sort_by_key(|m| MODIFIER_ORDER.iter().position(|c| c == m));
        // Without ⌃ or ⌘ every binding is a plain ⌥ combination, and those are
        // how macOS types characters: é, ñ, “ — and on German or Polish layouts
        // @, [, { and |. The menu would take them from every text field.
        if !modifiers.iter().any(|m| *m == "Ctrl" || *m == "Cmd") {
            return Err("The super key needs ⌃ or ⌘".into());
        }

        let mut keys = HashMap::new();
        let mut taken: HashMap<String, &'static str> = HashMap::new();
        for a in ACTIONS {
            let Some(raw) = self.keys.get(a.id) else { continue };
            let key = raw.trim().to_ascii_uppercase();
            if !key.is_empty() {
                if key.chars().count() != 1 || !ALLOWED_KEYS.contains(key.as_str()) {
                    return Err(format!("Unsupported key for \"{}\": {}", a.label, raw));
                }
                if let Some(other) = taken.insert(key.clone(), a.label) {
                    return Err(format!("\"{}\" and \"{}\" share the key {}", other, a.label, key));
                }
            }
            keys.insert(a.id.to_string(), key);
        }
        for a in ACTIONS {
            if keys.contains_key(a.id) {
                continue;
            }
            let key = if taken.contains_key(a.default_key) {
                String::new()
            } else {
                taken.insert(a.default_key.to_string(), a.label);
                a.default_key.to_string()
            };
            keys.insert(a.id.to_string(), key);
        }

        Ok(Self {
            modifiers: modifiers.into_iter().map(String::from).collect(),
            keys,
        })
    }

    /// Accelerator string for an action, or `None` when it is unbound.
    fn accelerator(&self, action_id: &str) -> Option<String> {
        let key = self.keys.get(action_id).filter(|k| !k.is_empty())?;
        Some(format!("{}+{}", self.modifiers.join("+"), key))
    }

    /// Unbinds every action whose combination is the global hotkey. Only for
    /// a keymap nobody chose just now — the saved or default one at startup;
    /// an edit is refused instead, so the user sees why.
    fn without_global(mut self, global: &str) -> Self {
        while let Some(label) = collides_with_global(&self, global) {
            let a = ACTIONS.iter().find(|a| a.label == label).expect("label comes from ACTIONS");
            log::warn!("[hotkeys] \"{}\" unbound: its combination is the global hotkey {}", label, global);
            self.keys.insert(a.id.to_string(), String::new());
        }
        self
    }
}

pub struct KeymapState(pub Mutex<Keymap>);

/// Menu bar state that outlives one `build_menu`.
#[derive(Default)]
pub struct MenuState {
    /// The silence toggle, retitled in place when silence mode changes instead
    /// of rebuilding the whole menu bar.
    silence_item: Mutex<Option<MenuItem<Wry>>>,
    /// Set while the Hotkeys editor records a key. Menu key equivalents are
    /// resolved before the page sees the keystroke, so pressing a bound
    /// combination would run the action instead of being recorded.
    suspended: AtomicBool,
}

/// Same combination, whatever the spelling — `Super+Shift+S` vs `Cmd+Shift+S`.
fn same_combination(a: &str, b: &str) -> bool {
    match (a.parse::<Shortcut>(), b.parse::<Shortcut>()) {
        (Ok(a), Ok(b)) => a.mods == b.mods && a.key == b.key,
        _ => false,
    }
}

/// The global show/hide hotkey is registered with the OS and fires before the
/// menu ever sees the key, so an in-app action bound to the same combination
/// could never run. Returns the label of the colliding action, if any.
fn collides_with_global(keymap: &Keymap, global: &str) -> Option<&'static str> {
    if global.is_empty() {
        return None;
    }
    ACTIONS.iter().find_map(|a| {
        let accel = keymap.accelerator(a.id)?;
        same_combination(&accel, global).then_some(a.label)
    })
}

/// Loads the keymap from the store, falling back to the defaults (with a
/// warning) if the saved value is unreadable or no longer valid.
///
/// The global hotkey was saved independently and may predate the keymap (an
/// upgrade from a release without in-app hotkeys), so a collision is resolved
/// here: the global hotkey fires first, and the shadowed action would sit in
/// the menu unreachable while every later `set_keymap` got refused over it.
pub fn load_keymap(store: &Store<Wry>, global: &str) -> Keymap {
    let keymap = match store.get(KEYMAP_STORE_KEY) {
        None => Keymap::default(),
        Some(value) => serde_json::from_value::<Keymap>(value)
            .map_err(|e| e.to_string())
            .and_then(Keymap::normalized)
            .unwrap_or_else(|e| {
                log::warn!("[hotkeys] saved keymap rejected, using defaults: {}", e);
                Keymap::default()
            }),
    };
    keymap.without_global(global)
}

// ── Sidebar order ───────────────────────────────────────────────────────────

/// One sidebar item, in the order the sidebar renders them: built-in
/// messengers, then user messengers, then custom shortcuts. `App.vue` numbers
/// its icons by the same order — keep the two in step.
enum Entry {
    Builtin { label: &'static str, name: &'static str },
    Custom { label: String, id: StoredId, url: String, name: String, is_shortcut: bool },
}

impl Entry {
    fn webview_label(&self) -> &str {
        match self {
            Entry::Builtin { label, .. } => label,
            Entry::Custom { label, .. } => label,
        }
    }

    fn name(&self) -> &str {
        match self {
            Entry::Builtin { name, .. } => name,
            Entry::Custom { name, .. } => name,
        }
    }
}

fn sidebar_entries(app: &AppHandle) -> Vec<Entry> {
    let mut entries: Vec<Entry> = MESSENGERS
        .iter()
        .map(|m| Entry::Builtin { label: m.label, name: m.display_name })
        .collect();
    if let Some(state) = app.try_state::<UserMessengers>() {
        entries.extend(state.0.lock().unwrap().iter().map(|m| Entry::Custom {
            label: m.webview_label(),
            id: m.id.clone(),
            url: m.url.clone(),
            name: m.name.clone(),
            is_shortcut: false,
        }));
    }
    if let Some(state) = app.try_state::<CustomShortcuts>() {
        entries.extend(state.0.lock().unwrap().iter().map(|sc| Entry::Custom {
            label: sc.webview_label(),
            id: sc.id.clone(),
            url: sc.url.clone(),
            name: sc.name.clone(),
            is_shortcut: true,
        }));
    }
    entries
}

fn active_label(app: &AppHandle) -> String {
    app.state::<ActiveMessenger>().0.lock().unwrap().clone()
}

/// Shows the sidebar item whose webview label is `label`. The tray menu uses
/// this too, so a hotkey and a tray click switch views the same way.
pub fn activate_label(app: &AppHandle, label: &str) {
    if let Some(entry) = sidebar_entries(app).into_iter().find(|e| e.webview_label() == label) {
        activate_entry(app, entry);
    }
}

/// Brings the main window forward and makes `entry` the visible view. A menu
/// key equivalent also fires while a dialog window is key, so the main window
/// may be behind it or hidden.
fn activate_entry(app: &AppHandle, entry: Entry) {
    if let Some(window) = app.get_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = match entry {
            Entry::Builtin { label, .. } => ensure_messenger_webview(app, label.to_string(), true).await,
            Entry::Custom { id, url, .. } => ensure_custom_webview(app, id, url, true).await,
        };
        if let Err(e) = result {
            log::warn!("[hotkeys] failed to switch view: {}", e);
        }
    });
}

fn step_entry(app: &AppHandle, forward: bool) {
    let mut entries = sidebar_entries(app);
    if entries.is_empty() {
        return;
    }
    let active = active_label(app);
    let len = entries.len();
    let next = match entries.iter().position(|e| e.webview_label() == active) {
        Some(i) if forward => (i + 1) % len,
        Some(i) => (i + len - 1) % len,
        None => 0,
    };
    activate_entry(app, entries.swap_remove(next));
}

// ── Actions ─────────────────────────────────────────────────────────────────

fn spawn_logged<F>(what: &'static str, fut: F)
where
    F: std::future::Future<Output = Result<(), String>> + Send + 'static,
{
    tauri::async_runtime::spawn(async move {
        if let Err(e) = fut.await {
            log::warn!("[hotkeys] {} failed: {}", what, e);
        }
    });
}

fn run_action(app: &AppHandle, action_id: &str) {
    if let Some(slot) = goto_slot(action_id) {
        let mut entries = sidebar_entries(app);
        if slot < entries.len() {
            activate_entry(app, entries.swap_remove(slot));
        }
        return;
    }
    match action_id {
        "next" => step_entry(app, true),
        "prev" => step_entry(app, false),
        "first-unread" => {
            let counts = app.state::<UnreadCounts>().0.lock().unwrap().clone();
            let target = sidebar_entries(app)
                .into_iter()
                .find(|e| counts.get(e.webview_label()).copied().unwrap_or(0) > 0);
            if let Some(entry) = target {
                activate_entry(app, entry);
            }
        }
        "add-shortcut" => spawn_logged("add-shortcut", open_add_shortcut_window(app.clone())),
        "add-messenger" => spawn_logged("add-messenger", open_add_messenger_window(app.clone())),
        "edit-shortcut" => {
            let active = active_label(app);
            let target = sidebar_entries(app).into_iter().find_map(|e| match e {
                Entry::Custom { label, id, is_shortcut: true, .. }
                    if label == active && is_safe_shortcut_id(id.as_str()) =>
                {
                    Some(ShortcutId(id.as_str().to_string()))
                }
                _ => None,
            });
            if let Some(id) = target {
                spawn_logged("edit-shortcut", open_edit_shortcut_window(app.clone(), id));
            }
        }
        "reload" => {
            let active = active_label(app);
            if let Some(webview) = app.get_webview(&active) {
                if let Err(e) = webview.reload() {
                    log::warn!("[hotkeys] reload of {} failed: {}", active, e);
                }
            }
        }
        "toggle-silence" => {
            let next = !*app.state::<SilenceMode>().0.lock().unwrap();
            if let Err(e) = apply_silence_mode(app, next) {
                log::warn!("[hotkeys] toggling silence mode failed: {}", e);
            }
        }
        "hotkeys" => spawn_logged("hotkeys", open_hotkeys_window(app.clone())),
        "cheatsheet" => spawn_logged("cheatsheet", open_cheatsheet_window(app.clone())),
        _ => log::warn!("[hotkeys] unknown action: {}", action_id),
    }
}

/// App-wide menu event handler. Ignores every id this module does not own —
/// the tray menu reports through the same channel.
pub fn handle_menu_event(app: &AppHandle, event: MenuEvent) {
    if let Some(action_id) = event.id.as_ref().strip_prefix(MENU_ID_PREFIX) {
        run_action(app, action_id);
    }
}

// ── Menu bar ────────────────────────────────────────────────────────────────

fn menu_label(app: &AppHandle, a: &HotkeyAction, entries: &[Entry]) -> Option<String> {
    if let Some(slot) = goto_slot(a.id) {
        return entries.get(slot).map(|e| e.name().to_string());
    }
    let label = match a.id {
        "toggle-silence" => silence_title(*app.state::<SilenceMode>().0.lock().unwrap()),
        _ => a.label,
    };
    Some(label.to_string())
}

fn silence_title(silenced: bool) -> &'static str {
    if silenced { "Turn silence mode off" } else { "Turn silence mode on" }
}

/// Retitles the silence toggle — its title names the state it switches to.
pub fn update_silence_item(app: &AppHandle, silenced: bool) {
    if let Some(item) = app.state::<MenuState>().silence_item.lock().unwrap().as_ref() {
        if let Err(e) = item.set_text(silence_title(silenced)) {
            log::warn!("[hotkeys] failed to retitle the silence item: {}", e);
        }
    }
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let keymap = app.state::<KeymapState>().0.lock().unwrap().clone();
    let entries = sidebar_entries(app);
    let menu_state = app.state::<MenuState>();
    let suspended = menu_state.suspended.load(Ordering::SeqCst);
    let mut silence_item = None;

    let navigate = Submenu::new(app, "Navigate", true)?;
    // Group boundaries: after the goto block, after first-unread, after
    // edit-shortcut, after toggle-silence.
    let separators_after = ["goto-9", "first-unread", "edit-shortcut", "toggle-silence"];
    for a in ACTIONS {
        if let Some(label) = menu_label(app, a, &entries) {
            let accel = if suspended { None } else { keymap.accelerator(a.id) };
            let item = MenuItem::with_id(
                app,
                format!("{}{}", MENU_ID_PREFIX, a.id),
                label,
                true,
                accel.as_deref(),
            )?;
            navigate.append(&item)?;
            if a.id == "toggle-silence" {
                silence_item = Some(item);
            }
        }
        if separators_after.contains(&a.id) {
            navigate.append(&PredefinedMenuItem::separator(app)?)?;
        }
    }

    // Starting from Tauri's default menu keeps Edit (copy/paste inside every
    // webview), Hide/Quit and Window exactly as they were before this menu
    // existed. "Navigate" goes right before "Window", per macOS convention.
    let menu = Menu::default(app)?;
    let window_index = menu
        .items()?
        .iter()
        .position(|item| item.as_submenu().is_some_and(|s| s.text().is_ok_and(|t| t == "Window")));
    match window_index {
        Some(i) => menu.insert(&navigate, i)?,
        None => menu.append(&navigate)?,
    }
    *menu_state.silence_item.lock().unwrap() = silence_item;
    Ok(menu)
}

/// Rebuilds the menu bar. Called whenever something it shows changes: the
/// keymap and the sidebar lists (names and order label the goto items). Silence
/// mode only retitles one item — see `update_silence_item`.
///
/// The Hotkeys editor and the cheatsheet render the same data, so they are
/// told to reload too.
///
/// Always deferred to a fresh main-thread turn: callers include the menu's own
/// event handler, and swapping the menu bar out from under the item that is
/// still being dispatched is not something to rely on.
pub fn refresh_menu(app: &AppHandle) {
    let handle = app.clone();
    let dispatched = app.run_on_main_thread(move || match build_menu(&handle) {
        Ok(menu) => {
            if let Err(e) = handle.set_menu(menu) {
                log::error!("[hotkeys] failed to set the menu: {}", e);
            }
        }
        Err(e) => log::error!("[hotkeys] failed to build the menu: {}", e),
    });
    if let Err(e) = dispatched {
        log::error!("[hotkeys] failed to schedule a menu rebuild: {}", e);
    }
    notify_hotkey_views(app);
}

/// Tells the Hotkeys editor and the cheatsheet, if open, to reload. Addressed
/// to those two windows only — a plain `emit` would reach every embedded page.
pub fn notify_hotkey_views(app: &AppHandle) {
    for label in ["hotkeys", "hotkey-cheatsheet"] {
        let _ = app.emit_to(EventTarget::webview_window(label), "hotkeys-changed", ());
    }
}

fn set_suspended(app: &AppHandle, suspend: bool) {
    if app.state::<MenuState>().suspended.swap(suspend, Ordering::SeqCst) == suspend {
        return;
    }
    // The global hotkey is taken by the OS before any window sees the key, so
    // it is released for the recording as well — otherwise re-recording it
    // would just hide the window.
    let global = app.state::<HotkeyConfig>().0.lock().unwrap().clone();
    if !global.is_empty() {
        let shortcuts = app.global_shortcut();
        if suspend {
            let _ = shortcuts.unregister(global.as_str());
        } else if !shortcuts.is_registered(global.as_str()) {
            if let Err(e) = register_toggle_shortcut(app, &global) {
                log::error!("[hotkeys] failed to restore the global hotkey {}: {}", global, e);
            }
        }
    }
    refresh_menu(app);
}

// ── Commands ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeysView {
    keymap: Keymap,
    default_keymap: Keymap,
    actions: &'static [HotkeyAction],
    global_hotkey: String,
    /// Sidebar item names in sidebar order; the goto rows show them.
    entries: Vec<String>,
}

#[tauri::command]
pub fn get_hotkeys(app: AppHandle) -> HotkeysView {
    HotkeysView {
        keymap: app.state::<KeymapState>().0.lock().unwrap().clone(),
        default_keymap: Keymap::default(),
        actions: ACTIONS,
        global_hotkey: app.state::<HotkeyConfig>().0.lock().unwrap().clone(),
        entries: sidebar_entries(&app).iter().map(|e| e.name().to_string()).collect(),
    }
}

#[tauri::command]
pub fn set_keymap(app: AppHandle, state: State<KeymapState>, keymap: Keymap) -> Result<Keymap, String> {
    let keymap = keymap.normalized()?;
    let global = app.state::<HotkeyConfig>().0.lock().unwrap().clone();
    if let Some(label) = collides_with_global(&keymap, &global) {
        return Err(format!("\"{}\" would use the global show/hide hotkey", label));
    }
    let store = app.store(SETTINGS_STORE).map_err(|e| e.to_string())?;
    store.set(KEYMAP_STORE_KEY, serde_json::to_value(&keymap).map_err(|e| e.to_string())?);
    store.save().map_err(|e| e.to_string())?;
    *state.0.lock().unwrap() = keymap.clone();
    refresh_menu(&app);
    Ok(keymap)
}

/// Suspends every hotkey while the editor records a key, and restores them.
/// Closing the editor restores them too (see `open_hotkeys_window`).
#[tauri::command]
pub fn suspend_hotkeys(app: AppHandle, suspend: bool) {
    set_suspended(&app, suspend);
}

/// Rejects a new global hotkey that an in-app action already uses. Called by
/// `set_global_shortcut` before it touches the registration.
pub fn check_global_hotkey(app: &AppHandle, shortcut: &str) -> Result<(), String> {
    let keymap = app.state::<KeymapState>().0.lock().unwrap().clone();
    match collides_with_global(&keymap, shortcut) {
        Some(label) => Err(format!("Already used by \"{}\"", label)),
        None => Ok(()),
    }
}

#[tauri::command]
pub async fn open_hotkeys_window(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("hotkeys") {
        let _ = win.set_focus();
        return Ok(());
    }
    let win = dialog_window(&app, "hotkeys", "index.html?view=hotkeys".into(), "Hotkeys", 440.0, 640.0)
        .min_inner_size(400.0, 480.0)
        .resizable(true)
        .build()
        .map_err(|e| e.to_string())?;
    // A window closed mid-recording never sends the resume.
    win.on_window_event(move |event| {
        if matches!(event, WindowEvent::Destroyed) {
            set_suspended(&app, false);
        }
    });
    Ok(())
}

/// Read-only list of every binding. Pressing its own hotkey again, or Esc,
/// closes it.
#[tauri::command]
pub async fn open_cheatsheet_window(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("hotkey-cheatsheet") {
        let _ = win.close();
        return Ok(());
    }
    dialog_window(&app, "hotkey-cheatsheet", "index.html?view=hotkey-cheatsheet".into(), "Hotkey Cheatsheet", 380.0, 600.0)
    .min_inner_size(340.0, 400.0)
    .resizable(true)
    .build()
    .map_err(|e| e.to_string())?;
    Ok(())
}
