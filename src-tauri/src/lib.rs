mod capture;
mod cli;
mod matte;

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
        .invoke_handler(tauri::generate_handler![
            capture::capture_page,
            capture::capture_page_pdf,
            matte::matte_infer
        ])
        .run(context)
        .expect("error while running Lolly desktop");
}
