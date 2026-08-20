mod capture;
mod cli;
mod matte;
mod native_transport;
mod nearby;
mod oauth;
mod reword;
mod site_fetch;

use tauri::menu::{Menu, MenuItem, Submenu};
use tauri::Manager;

/// Native entry (called from `main.rs`, and the mobile entry point). Reads argv
/// and dispatches to either the GUI or a headless CLI render.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    dispatch(std::env::args().skip(1).collect());
}

/// The single `generate_context!()` site (embedding the frontend assets happens
/// once here). Help/version are answered without ever building the app; GUI and
/// CLI both take the one embedded context.
fn dispatch(args: Vec<String>) {
    let mode = cli::classify(&args);

    // Answer help / version / usage errors without ever building the app.
    match &mode {
        cli::Mode::Help => return cli::print_help(),
        cli::Mode::Version => return cli::print_version(),
        cli::Mode::UsageError(m) => {
            eprintln!("lolly: {m}\n");
            cli::print_help();
            std::process::exit(64); // EX_USAGE
        }
        _ => {}
    }

    // One embed of the frontend assets, taken by whichever of GUI / CLI runs.
    let context = tauri::generate_context!();
    match mode {
        cli::Mode::Cli(job) => cli::run_cli(context, job),
        _ => run_gui(context), // Gui (the others already returned above)
    }
}

/// The desktop app proper: host the WebView and fulfil the `capture` capability.
fn run_gui(context: tauri::Context) {
    tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_http::init())
        // The shared authenticated-capture session (persistent Chrome profile + the
        // optional live sign-in browser captures ride). Managed here so both the
        // capture commands and the sign-in/clear commands see the same instance.
        .manage(capture::CaptureSession::default())
        // Loopback OAuth listeners (plans/129 WP4) — the system-browser sign-in
        // return leg for personal send targets. GUI only, like site_fetch.
        .manage(oauth::OauthListeners::default())
        // Add "Open Exports Folder" to the menu bar (exports land in ~/Downloads/Lolly —
        // bridge-overrides/export.ts). Placed in the Window menu (Andy's ask); falls back
        // to a top-level "Exports" menu if a platform's default menu has no Window submenu.
        .menu(|handle| {
            let menu = Menu::default(handle)?;
            let open_exports =
                MenuItem::with_id(handle, "open_exports", "Open Exports Folder", true, None::<&str>)?;
            let mut placed = false;
            if let Ok(items) = menu.items() {
                for item in &items {
                    if let Some(sub) = item.as_submenu() {
                        if sub.text().ok().as_deref() == Some("Window") {
                            let _ = sub.append(&open_exports);
                            placed = true;
                            break;
                        }
                    }
                }
            }
            if !placed {
                menu.append(&Submenu::with_items(handle, "Exports", true, &[&open_exports])?)?;
            }
            Ok(menu)
        })
        .on_menu_event(|app, event| {
            if event.id() == "open_exports" {
                open_exports_folder(app);
            }
        })
        .invoke_handler(tauri::generate_handler![
            capture::capture_page,
            capture::capture_page_pdf,
            // Authenticated capture: open a visible sign-in window on the shared
            // profile, report whether a session is live, and clear it. GUI only —
            // interactive, like site_fetch/nearby; a headless render never signs in.
            capture::capture_signin_open,
            capture::capture_session_active,
            capture::capture_clear_session,
            matte::matte_infer,
            // Native reword (plans/127): the SmolLM2 sampling loop on native ORT —
            // probe/stage/generate; the JS side owns consent + the engine gate.
            // GUI only, like site_fetch: it is reachable only from the catalog's
            // humanize panel, never from a headless render.
            reword::reword_probe,
            reword::reword_put_file,
            reword::reword_generate,
            // Website source for the Design System studio (plans/97 section 9). GUI
            // only, deliberately: unlike capture, which cli.rs also registers
            // because the url-shot TOOL calls host.capture mid-render, this
            // command is reachable only from a button in the studio. A headless
            // `Lolly run <tool>` render can never invoke it, so registering it
            // there would be dead surface.
            site_fetch::site_fetch,
            // Loopback OAuth (plans/129 WP4): bind an ephemeral 127.0.0.1 port,
            // then hand back the one redirect the system browser delivers. GUI
            // only — sign-in is interactive by definition.
            oauth::oauth_listen,
            oauth::oauth_wait,
            // Nearby discovery (plans/110 section 3). GUI only for the same reason as
            // site_fetch: it is reachable from the collab ceremony / share sheets,
            // never from a headless render, so it stays out of cli.rs's handler.
            nearby::nearby_set_visible,
            nearby::nearby_hide,
            nearby::nearby_browse,
            nearby::nearby_poll,
            nearby::nearby_exchange_invite,
            nearby::nearby_send_reply,
            nearby::nearby_decline,
            // Native LAN socket transport (plans/110 section 4) — GUI only, same reason.
            native_transport::native_connect,
            native_transport::native_send,
            native_transport::native_recv,
            native_transport::native_plate,
            native_transport::native_close,
            native_transport::native_poll_inbound,
            native_transport::native_adopt
        ])
        .run(context)
        .expect("error while running Lolly desktop");
}

/// Reveal the exports folder in the OS file manager. Exports save to
/// `~/Downloads/Lolly` (bridge-overrides/export.ts `saveToDownloads`); create it if the
/// user has not exported anything yet, then open it. Best-effort — a menu click never errors.
fn open_exports_folder(app: &tauri::AppHandle) {
    let Ok(downloads) = app.path().download_dir() else {
        return;
    };
    let dir = downloads.join("Lolly");
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(&dir).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(&dir).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
}
