mod capture;
mod cli;
mod desktop_integration;
mod menu;
mod native_transport;
mod nearby;
mod oauth;
mod reword;
mod site_fetch;

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
        // One running app per user (plans/174): a second launch (file-manager
        // "Open with", a lolly:// click) forwards its argv here and exits; the
        // classifier turns it into openFile/deepLink events the webview polls.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            desktop_integration::classify_argv(app, &argv[1..]);
        }))
        .manage(desktop_integration::DesktopEvents::default())
        .manage(desktop_integration::HotFolder::default())
        // The shared authenticated-capture session (persistent Chrome profile + the
        // optional live sign-in browser captures ride). Managed here so both the
        // capture commands and the sign-in/clear commands see the same instance.
        .manage(capture::CaptureSession::default())
        // Loopback OAuth listeners (plans/129 WP4) - the system-browser sign-in
        // return leg for personal send targets. GUI only, like site_fetch.
        .manage(oauth::OauthListeners::default())
        // The full native app menu (menu.rs): Go/Tools/Appearance/Help mirroring
        // the iPad menu bar, plus the pre-existing "Open Exports Folder" in the
        // Window menu. Starts with static content; the frontend's set_menu_data
        // push fills in the dynamic parts (folders, utilities, theme state).
        .menu(|handle| menu::build_menu(handle, &menu::MenuData::default()))
        .on_menu_event(|app, event| menu::handle_event(app, &event))
        .invoke_handler(tauri::generate_handler![
            desktop_integration::desktop_poll_events,
            desktop_integration::desktop_read_file,
            desktop_integration::desktop_pick_color,
            desktop_integration::desktop_set_wallpaper,
            desktop_integration::desktop_set_wallpaper_bytes,
            desktop_integration::desktop_read_accent,
            desktop_integration::desktop_clipboard_read,
            desktop_integration::desktop_hotfolder_set,
            menu::set_menu_data,
            capture::capture_page,
            capture::capture_page_pdf,
            // Authenticated capture: open a visible sign-in window on the shared
            // profile, report whether a session is live, and clear it. GUI only -
            // interactive, like site_fetch/nearby; a headless render never signs in.
            capture::capture_signin_open,
            capture::capture_session_active,
            capture::capture_clear_session,
            // Native reword (plans/127): the SmolLM2 sampling loop on native ORT -
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
            // only - sign-in is interactive by definition.
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
            // Native LAN socket transport (plans/110 section 4) - GUI only, same reason.
            native_transport::native_connect,
            native_transport::native_send,
            native_transport::native_recv,
            native_transport::native_plate,
            native_transport::native_close,
            native_transport::native_poll_inbound,
            native_transport::native_adopt
        ])
        // Desktop integration boot (plans/174): first-launch argv (a .lolly
        // double-clicked before the app ran, a lolly:// link that launched us),
        // the clipboard-lens tray, and - on Linux - the D-Bus search/automation
        // surfaces. All additive; failures degrade to a plain window, logged.
        .setup(|app| {
            let handle = app.handle().clone();
            desktop_integration::classify_argv(
                &handle,
                &std::env::args().skip(1).collect::<Vec<_>>(),
            );
            if let Err(e) = desktop_integration::setup_tray(&handle) {
                eprintln!("[desktop] tray unavailable: {e}");
            }
            #[cfg(target_os = "linux")]
            desktop_integration::dbus::serve(handle);
            Ok(())
        })
        .run(context)
        .expect("error while running Lolly desktop");
}

