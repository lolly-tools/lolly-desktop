// SPDX-License-Identifier: MPL-2.0
//! Linux desktop integration (plans/174): XDG portals, the clipboard-lens tray,
//! single-instance argv routing, a hot-folder watcher, and the D-Bus surfaces
//! (GNOME Shell search, KRunner, org.lolly.Desktop1).
//!
//! Rust->JS follows the house POLL pattern (nearby.rs precedent - no event
//! plumbing exists in this codebase): everything that happens natively lands in
//! one `DesktopEvents` queue, and the web side drains it via
//! `desktop_poll_events` every 1200ms (shells/web/src/lib/linux-desktop-boot.ts).
//!
//! Portal calls (colour picker, wallpaper, accent) are Linux-only; on other
//! platforms - and on Linux sessions with no xdg-desktop-portal - the commands
//! return Err("unsupported"), which the TS side treats as feature-absent, never
//! as a failure to show the user.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

/// One queued native happening. `kind` is the plan-174 contract vocabulary:
/// "openFile" | "deepLink" | "hotfolderFile" | "navigate".
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopEvent {
    pub kind: String,
    pub value: String,
}

/// The queue the webview polls, plus the custody set for file reads: the
/// webview's fs scope is deliberately narrow (appdata + downloads), so a file
/// the OS hands us by path (double-clicked .lolly in ~/Documents, a hot-folder
/// arrival) is served back through `desktop_read_file`, which only ever reads
/// paths THIS module queued. That keeps "the app can open what you opened"
/// without granting "the webview can read your home directory".
#[derive(Default)]
pub struct DesktopEvents {
    queue: Mutex<Vec<DesktopEvent>>,
    delivered_paths: Mutex<HashSet<PathBuf>>,
}

impl DesktopEvents {
    fn push(&self, kind: &str, value: String) {
        if let Ok(mut q) = self.queue.lock() {
            // A stuck (never-polling) webview must not grow memory without
            // bound from a chatty hot folder; the newest events win.
            if q.len() > 256 {
                q.drain(0..128);
            }
            q.push(DesktopEvent { kind: kind.into(), value });
        }
    }

    fn push_path(&self, kind: &str, path: PathBuf) {
        if let Ok(mut p) = self.delivered_paths.lock() {
            p.insert(path.clone());
        }
        self.push(kind, path.to_string_lossy().into_owned());
    }
}

/// The live hot-folder watcher, replaced wholesale on every `desktop_hotfolder_set`.
#[derive(Default)]
pub struct HotFolder(Mutex<Option<notify::RecommendedWatcher>>);

// ── argv routing (single-instance + first launch) ────────────────────────────

/// Classify launch arguments into events. Both the first process and every
/// forwarded second-instance argv go through here, so "double-click a .lolly"
/// and "click a lolly:// link" behave identically however the app was started.
pub fn classify_argv(app: &AppHandle, argv: &[String]) {
    let events: State<'_, DesktopEvents> = app.state();
    for arg in argv {
        if arg.starts_with("lolly://") {
            events.push("deepLink", arg.clone());
            continue;
        }
        // Anything that names an existing file is an open request; the web
        // side sniffs the flavour (.lolly share/brand vs image vs pdf) exactly
        // as it does for a drag-drop.
        let path = PathBuf::from(arg);
        if path.is_file() {
            match path.canonicalize() {
                Ok(canon) => events.push_path("openFile", canon),
                Err(_) => events.push_path("openFile", path),
            }
        }
    }
    focus_main(app);
}

pub fn focus_main(app: &AppHandle) {
    if let Some(win) = app.webview_windows().values().next() {
        let _ = win.show();
        let _ = win.set_focus();
    }
}

// ── the poll commands (the whole Rust->JS surface) ───────────────────────────

#[tauri::command]
pub fn desktop_poll_events(events: State<'_, DesktopEvents>) -> Vec<DesktopEvent> {
    events.queue.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
}

/// Serve a file the OS delivered by path (see DesktopEvents doc). Refuses any
/// path that never travelled through the event queue.
#[tauri::command]
pub fn desktop_read_file(events: State<'_, DesktopEvents>, path: String) -> Result<Vec<u8>, String> {
    let p = PathBuf::from(&path);
    let allowed = events
        .delivered_paths
        .lock()
        .map(|s| s.contains(&p))
        .unwrap_or(false);
    if !allowed {
        return Err("path was not delivered by the desktop".into());
    }
    std::fs::read(&p).map_err(|e| e.to_string())
}

// ── portals (Linux) ──────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn to_hex(c: &ashpd::desktop::Color) -> String {
    let ch = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", ch(c.red()), ch(c.green()), ch(c.blue()))
}

#[tauri::command]
pub async fn desktop_pick_color() -> Result<String, String> {
    #[cfg(target_os = "linux")]
    {
        let color = ashpd::desktop::Color::pick()
            .send()
            .await
            .map_err(|e| e.to_string())?
            .response()
            .map_err(|e| e.to_string())?;
        return Ok(to_hex(&color));
    }
    #[cfg(not(target_os = "linux"))]
    Err("unsupported".into())
}

#[tauri::command]
pub async fn desktop_set_wallpaper(path: String, target: String) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        use ashpd::desktop::wallpaper::{SetOn, WallpaperRequest};
        let set_on = match target.as_str() {
            "lockscreen" => SetOn::Lockscreen,
            "both" => SetOn::Both,
            _ => SetOn::Background,
        };
        let uri = url::Url::from_file_path(&path)
            .map_err(|_| format!("not an absolute file path: {path}"))?;
        let uri = ashpd::Uri::parse(uri.as_str()).map_err(|e| e.to_string())?;
        // show_preview(true): the PORTAL confirms with the user - the app never
        // changes someone's desktop silently.
        WallpaperRequest::default()
            .set_on(set_on)
            .show_preview(true)
            .build_uri(&uri)
            .await
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, target);
        Err("unsupported".into())
    }
}

/// Wallpaper straight from export bytes: the send driver hands us the rendered
/// image, we stage it in the app cache dir (portals need a file/uri) and ask
/// the portal - which previews and confirms with the user - to set it.
#[tauri::command]
pub async fn desktop_set_wallpaper_bytes(
    app: AppHandle,
    bytes: Vec<u8>,
    ext: String,
    target: String,
) -> Result<(), String> {
    let safe_ext = match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" => ext.as_str(),
        _ => "png",
    };
    let dir = app.path().app_cache_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("wallpaper.{safe_ext}"));
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    desktop_set_wallpaper(path.to_string_lossy().into_owned(), target).await
}

#[tauri::command]
pub async fn desktop_read_accent() -> Result<Option<String>, String> {
    #[cfg(target_os = "linux")]
    {
        let settings = match ashpd::desktop::settings::Settings::new().await {
            Ok(s) => s,
            Err(_) => return Ok(None), // no portal = no accent, not an error
        };
        return Ok(settings.accent_color().await.ok().map(|c| to_hex(&c)));
    }
    #[cfg(not(target_os = "linux"))]
    Ok(None)
}

// ── clipboard (tray + lens UI, one-shot reads only) ──────────────────────────

/// One-shot clipboard read. Only ever called from an explicit user gesture (a
/// tray item click, the lens UI button) - this module deliberately has NO
/// clipboard watcher, so nothing is observed that the user did not just ask
/// about. That is the privacy stance, not an implementation shortcut.
#[tauri::command]
pub fn desktop_clipboard_read() -> Result<String, String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.get_text().map_err(|e| e.to_string())
}

// ── hot folder ───────────────────────────────────────────────────────────────

#[tauri::command]
pub fn desktop_hotfolder_set(
    app: AppHandle,
    hot: State<'_, HotFolder>,
    path: Option<String>,
) -> Result<(), String> {
    use notify::Watcher;
    let mut slot = hot.0.lock().map_err(|e| e.to_string())?;
    // Dropping the previous watcher stops it; replacement is wholesale.
    *slot = None;
    let Some(path) = path else { return Ok(()) };
    let dir = PathBuf::from(&path);
    if !dir.is_dir() {
        return Err(format!("not a directory: {path}"));
    }
    let handle = app.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        // Create + moved-into cover "a file arrived" across file managers,
        // browsers (rename-into-place) and `mv`.
        let arrived = matches!(
            event.kind,
            notify::EventKind::Create(_)
                | notify::EventKind::Modify(notify::event::ModifyKind::Name(
                    notify::event::RenameMode::To
                ))
        );
        if !arrived {
            return;
        }
        let events: State<'_, DesktopEvents> = handle.state();
        for p in event.paths {
            if p.is_file() {
                events.push_path("hotfolderFile", p);
            }
        }
    })
    .map_err(|e| e.to_string())?;
    watcher
        .watch(&dir, notify::RecursiveMode::NonRecursive)
        .map_err(|e| e.to_string())?;
    *slot = Some(watcher);
    Ok(())
}

// ── the clipboard-lens tray ──────────────────────────────────────────────────

/// A static menu whose items classify the clipboard AT CLICK TIME (one gesture,
/// one read - see desktop_clipboard_read's doc). Building the menu contents
/// from the clipboard at open time would read it on every hover; the static
/// shape trades a little polish for never touching the clipboard uninvited.
pub fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder};
    use tauri::tray::TrayIconBuilder;

    let qr = MenuItemBuilder::with_id("lens-qr", "QR code from clipboard").build(app)?;
    let color = MenuItemBuilder::with_id("lens-color", "Colour Lab from clipboard").build(app)?;
    let open = MenuItemBuilder::with_id("open", "Open Lolly").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
    let menu = MenuBuilder::new(app)
        .items(&[&qr, &color, &open, &quit])
        .build()?;

    TrayIconBuilder::with_id("lolly-tray")
        .icon(app.default_window_icon().cloned().ok_or(tauri::Error::WindowNotFound)?)
        .tooltip("Lolly")
        .menu(&menu)
        .on_menu_event(|app, event| {
            let nav = |route: String| {
                let events: State<'_, DesktopEvents> = app.state();
                events.push("navigate", route);
                focus_main(app);
            };
            match event.id().as_ref() {
                "open" => focus_main(app),
                "quit" => app.exit(0),
                "lens-qr" => {
                    let text = desktop_clipboard_read().unwrap_or_default();
                    let text = text.trim();
                    if text.is_empty() {
                        focus_main(app);
                    } else {
                        nav(format!(
                            "#/tool/qr-code?url={}",
                            urlencode(&text.chars().take(2000).collect::<String>())
                        ));
                    }
                }
                "lens-color" => {
                    let text = desktop_clipboard_read().unwrap_or_default();
                    let t = text.trim();
                    let is_hex = t.starts_with('#')
                        && matches!(t.len(), 4 | 5 | 7 | 9)
                        && t[1..].chars().all(|c| c.is_ascii_hexdigit());
                    if is_hex {
                        nav(format!("#/lab?c={}", urlencode(t)));
                    } else {
                        nav("#/lab".into());
                    }
                }
                _ => {}
            }
        })
        .build(app)?;
    Ok(())
}

/// Minimal percent-encoding for hash-route query values - std-only on purpose
/// (the full URL grammar lives web-side in engine url-mode; this only has to
/// keep `#`, `&`, `%`, `+` and spaces from breaking the hash route).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ── D-Bus: GNOME Shell search, KRunner, org.lolly.Desktop1 (Linux only) ──────

#[cfg(target_os = "linux")]
pub mod dbus {
    use super::{focus_main, DesktopEvents};
    use std::collections::HashMap;
    use tauri::{AppHandle, Manager, State};

    /// One searchable tool row, parsed from the catalog index EMBEDDED in the
    /// binary (frontendDist catalog/tools/index.json - the neutral/lolly-start
    /// set unless a profile build embedded more). Matching vocabulary mirrors
    /// the in-app provider (lib/search/providers/tools.ts): name 3, tags 2,
    /// id 2, description 1.
    #[derive(Clone)]
    struct ToolRow {
        id: String,
        name: String,
        description: String,
        haystacks: Vec<(String, u32)>,
    }

    #[derive(Clone)]
    pub struct Index(std::sync::Arc<Vec<ToolRow>>);

    impl Index {
        pub fn load(app: &AppHandle) -> Self {
            let rows = app
                .asset_resolver()
                .get("catalog/tools/index.json".into())
                .and_then(|asset| serde_json::from_slice::<serde_json::Value>(&asset.bytes()).ok())
                .and_then(|v| v.get("tools").cloned())
                .and_then(|t| t.as_array().cloned())
                .map(|tools| {
                    tools
                        .iter()
                        .filter_map(|t| {
                            let s = |k: &str| {
                                t.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string()
                            };
                            if t.get("listed").and_then(|v| v.as_bool()) == Some(false) {
                                return None;
                            }
                            let id = s("id");
                            if id.is_empty() {
                                return None;
                            }
                            let name = s("name");
                            let description = s("description");
                            let tags = t
                                .get("tags")
                                .and_then(|v| v.as_array())
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|x| x.as_str())
                                        .collect::<Vec<_>>()
                                        .join(" ")
                                })
                                .unwrap_or_default();
                            let haystacks = vec![
                                (name.to_lowercase(), 3),
                                (tags.to_lowercase(), 2),
                                (id.to_lowercase(), 2),
                                (description.to_lowercase(), 1),
                            ];
                            Some(ToolRow { id, name, description, haystacks })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Index(std::sync::Arc::new(rows))
        }

        fn matches(&self, terms: &[String], limit: usize) -> Vec<&ToolRow> {
            let terms: Vec<String> = terms
                .iter()
                .map(|t| t.to_lowercase())
                .filter(|t| !t.is_empty())
                .collect();
            if terms.is_empty() {
                return Vec::new();
            }
            let mut scored: Vec<(u32, &ToolRow)> = self
                .0
                .iter()
                .filter_map(|row| {
                    let mut score = 0u32;
                    for term in &terms {
                        let s: u32 = row
                            .haystacks
                            .iter()
                            .filter(|(hay, _)| hay.contains(term.as_str()))
                            .map(|(_, w)| *w)
                            .sum();
                        if s == 0 {
                            return None; // every term must hit somewhere
                        }
                        score += s;
                    }
                    Some((score, row))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
            scored.into_iter().take(limit).map(|(_, r)| r).collect()
        }
    }

    fn activate(app: &AppHandle, tool_id: &str) {
        let events: State<'_, DesktopEvents> = app.state();
        events.push("navigate", format!("#/tool/{tool_id}"));
        focus_main(app);
    }

    /// org.gnome.Shell.SearchProvider2 - result ids ARE tool ids.
    struct GnomeSearch {
        app: AppHandle,
        index: Index,
    }

    #[zbus::interface(name = "org.gnome.Shell.SearchProvider2")]
    impl GnomeSearch {
        fn get_initial_result_set(&self, terms: Vec<String>) -> Vec<String> {
            self.index.matches(&terms, 8).iter().map(|r| r.id.clone()).collect()
        }
        fn get_subsearch_result_set(
            &self,
            _previous: Vec<String>,
            terms: Vec<String>,
        ) -> Vec<String> {
            self.index.matches(&terms, 8).iter().map(|r| r.id.clone()).collect()
        }
        fn get_result_metas(
            &self,
            ids: Vec<String>,
        ) -> Vec<HashMap<String, zbus::zvariant::OwnedValue>> {
            ids.iter()
                .filter_map(|id| self.index.0.iter().find(|r| &r.id == id))
                .map(|row| {
                    let mut m: HashMap<String, zbus::zvariant::OwnedValue> = HashMap::new();
                    let put = |m: &mut HashMap<_, zbus::zvariant::OwnedValue>,
                               k: &str,
                               v: &str| {
                        m.insert(
                            k.to_string(),
                            zbus::zvariant::Value::from(v.to_string()).try_into().unwrap(),
                        );
                    };
                    put(&mut m, "id", &row.id);
                    put(&mut m, "name", &row.name);
                    put(&mut m, "description", &row.description);
                    put(&mut m, "gicon", "tools.lolly.Desktop");
                    m
                })
                .collect()
        }
        fn activate_result(&self, id: String, _terms: Vec<String>, _timestamp: u32) {
            activate(&self.app, &id);
        }
        fn launch_search(&self, _terms: Vec<String>, _timestamp: u32) {
            focus_main(&self.app);
        }
    }

    /// org.kde.krunner1 - the same matcher for Plasma. `type` 100 = exact-ish
    /// match band, relevance from our weights normalised to 0..1.
    struct KRunner {
        app: AppHandle,
        index: Index,
    }

    type KMatch = (String, String, String, i32, f64, HashMap<String, zbus::zvariant::OwnedValue>);

    #[zbus::interface(name = "org.kde.krunner1")]
    impl KRunner {
        #[zbus(name = "Match")]
        fn match_(&self, query: String) -> Vec<KMatch> {
            let terms: Vec<String> = query.split_whitespace().map(String::from).collect();
            self.index
                .matches(&terms, 8)
                .iter()
                .map(|r| {
                    (
                        r.id.clone(),
                        format!("Lolly: {}", r.name),
                        "tools.lolly.Desktop".to_string(),
                        100,
                        0.8,
                        HashMap::new(),
                    )
                })
                .collect()
        }
        fn actions(&self) -> Vec<(String, String, String)> {
            Vec::new()
        }
        fn run(&self, match_id: String, _action_id: String) {
            activate(&self.app, &match_id);
        }
    }

    /// org.lolly.Desktop1 - external automation. Render is v1 = open-in-app:
    /// the plan's escape hatch (a true headless render reuses cli.rs's hidden
    /// window and is follow-up work; shipping a broken renderer would be worse
    /// than an honest "opened").
    struct Desktop1 {
        app: AppHandle,
    }

    #[zbus::interface(name = "org.lolly.Desktop1")]
    impl Desktop1 {
        fn activate(&self) {
            focus_main(&self.app);
        }
        fn show_tool(&self, id: String) {
            activate(&self.app, &id);
        }
        fn render(&self, tool_url: String, _out_path: String) -> String {
            let events: State<'_, DesktopEvents> = self.app.state();
            let route = tool_url
                .strip_prefix("lolly://")
                .map(|rest| format!("#/{}", rest.trim_start_matches('/')))
                .unwrap_or(tool_url);
            events.push("navigate", route.clone());
            focus_main(&self.app);
            format!("opened:{route}")
        }
    }

    /// Own both bus names and serve the three interfaces. Failure is logged and
    /// swallowed: a session without a bus (containers, exotic setups) must not
    /// take the app down with it.
    pub fn serve(app: AppHandle) {
        let index = Index::load(&app);
        tauri::async_runtime::spawn(async move {
            let search = GnomeSearch { app: app.clone(), index: index.clone() };
            let krunner = KRunner { app: app.clone(), index };
            let desktop1 = Desktop1 { app: app.clone() };
            let build = zbus::connection::Builder::session()
                .and_then(|b| b.name("tools.lolly.Desktop.SearchProvider"))
                .and_then(|b| b.serve_at("/tools/lolly/Desktop/SearchProvider", search))
                .and_then(|b| b.serve_at("/tools/lolly/Desktop/SearchProvider", krunner));
            match build {
                Ok(b) => match b.build().await {
                    Ok(conn) => {
                        // Second well-known name on the same connection.
                        let served = conn
                            .object_server()
                            .at("/org/lolly/Desktop1", desktop1)
                            .await
                            .is_ok();
                        let named =
                            conn.request_name("org.lolly.Desktop1").await.is_ok();
                        if !(served && named) {
                            eprintln!("[desktop] org.lolly.Desktop1 not fully served");
                        }
                        // Keep the connection alive for the app's lifetime.
                        std::mem::forget(conn);
                    }
                    Err(e) => eprintln!("[desktop] dbus unavailable: {e}"),
                },
                Err(e) => eprintln!("[desktop] dbus setup failed: {e}"),
            }
        });
    }
}
