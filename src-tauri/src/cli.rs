// SPDX-License-Identifier: MPL-2.0
//! Command-line mode for the desktop binary.
//!
//! The macOS `.app` ships a single Mach-O at `Lolly.app/Contents/MacOS/Lolly`.
//! Run it with no arguments (or from Finder) and it opens the GUI, exactly as
//! before. Run it with a tool to render and it does the render HEADLESSLY and
//! exits - one binary, both a desktop app and a command line, which is the
//! Tauri-supported shape for this.
//!
//! There is no way to render a Lolly tool without a JavaScript runtime: the
//! engine renders into a DOM, and the desktop app's render path IS the web
//! shell running in a WebView. So "headless" here is not a second renderer in
//! Rust - it is the same web shell, driven through URL mode (`#/tool/<id>?…&export=1`,
//! which the shell auto-exports on load, see shells/web/src/views/tool.ts) in an
//! OFF-SCREEN window, with the finished bytes handed back to Rust to write to the
//! path the user asked for. The full-featured headless renderer with every
//! format tier, C2PA, batch, preflight and validate remains the Node CLI
//! (`shells/cli`, `npm run cli`); this is the subset that ships inside the app.
//!
//! Flow:
//!   1. `classify()` decides GUI vs. a CLI job from argv.
//!   2. `run_cli()` builds the app with the config window cleared (no visible
//!      window), creates one off-screen "main" window pointed at the tool URL,
//!      and injects `window.__LOLLY_CLI__` + an unknown-tool guard.
//!   3. The desktop export override (bridge-overrides/export.ts), seeing
//!      `window.__LOLLY_CLI__`, delivers the export bytes to `cli_write`
//!      instead of saving to Downloads, then calls `cli_done`.
//!   4. A watchdog thread bounds the whole thing so a stuck render can't hang
//!      the terminal forever.
//!
//! stdout carries the payload (only when `--output=-`); every diagnostic goes to
//! stderr, matching the Node CLI's contract.

use std::io::Write;

/// The window label. It is deliberately "main" so the existing capability set in
/// `capabilities/default.json` (`"windows": ["main"]`) - fs scope, http, core -
/// applies to the headless window too. A different label would boot the web shell
/// with no filesystem/network permission and every state/net call would throw.
const WINDOW_LABEL: &str = "main";

/// How long a headless render may take before we give up. Generous, because the
/// first boot warms the whole web shell (catalog sync, i18n, fonts) before the
/// render even starts. Override with `LOLLY_CLI_TIMEOUT=<seconds>`.
fn watchdog_secs() -> u64 {
    std::env::var("LOLLY_CLI_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(90)
}

/// A resolved CLI render job, shared with the invoke handlers via managed state.
#[derive(Clone)]
pub struct CliJob {
    /// Bare tool id (e.g. "qr-code") - carried for the unknown-tool guard message.
    pub tool_id: String,
    /// The URL-mode query, already encoded, without a leading `?`. Always carries
    /// `export=1` (the auto-export trigger) and, when given, `format=<fmt>`.
    pub query: String,
    /// Absolute or relative path to write the export to. `None` ⇒ derive from the
    /// tool's own filename, written into the process CWD (the terminal's CWD).
    pub output: Option<String>,
    /// `--output=-` ⇒ write the bytes to stdout instead of a file.
    pub stdout: bool,
}

/// What argv resolves to.
pub enum Mode {
    Gui,
    Help,
    Version,
    UsageError(String),
    Cli(CliJob),
}

/// Decide GUI vs. CLI from the arguments (argv without the program name).
///
/// GUI is the safe default: no arguments, or a leading flag we do not recognise
/// (macOS LaunchServices historically passes `-psn_0_…` when opened from Finder),
/// falls through to the window. A CLI job needs an explicit tool: either
/// `run <tool>` or a bare `<tool>` / pasted link as the first argument.
pub fn classify(args: &[String]) -> Mode {
    let Some(first) = args.first() else { return Mode::Gui };

    match first.as_str() {
        "--help" | "-h" | "help" => return Mode::Help,
        "--version" | "-V" => return Mode::Version,
        _ => {}
    }

    let (spec, flags): (&str, &[String]) = if first == "run" {
        match args.get(1) {
            Some(t) if !t.starts_with('-') => (t.as_str(), &args[2..]),
            _ => return Mode::UsageError("`run` needs a tool id, e.g. `run qr-code --url=…`".into()),
        }
    } else if first.starts_with('-') {
        // Unknown leading flag → GUI. Covers `-psn_…` and any future `--flag` that
        // isn't a CLI verb, so the window still opens rather than erroring.
        return Mode::Gui;
    } else if first.starts_with("lolly://") || std::path::Path::new(first).exists() {
        // A deep link or an existing file path is the DESKTOP handing us
        // something to open (plans/174: "Open With" from a file manager, a
        // double-clicked .lolly, an x-scheme-handler launch) - never a tool id.
        // GUI mode keeps the argv; run_gui's classify_argv turns it into
        // openFile/deepLink events, or forwards it to the running instance.
        // Without this branch the bare-tool sugar below ate the path and the
        // app died with `unknown tool "home/…/x.lolly"` - the first real
        // double-click found it (2026-08-30).
        return Mode::Gui;
    } else {
        // Bare-tool sugar: `Lolly qr-code --url=…` or a pasted lolly.tools link.
        (first.as_str(), &args[1..])
    };

    let (tool_id, spec_query) = parse_spec(spec);
    if tool_id.is_empty() {
        return Mode::UsageError(format!("could not read a tool id from \"{spec}\""));
    }

    // Merge, last-wins: pasted-link query first, CLI flags override it, then the
    // reserved controls. Keys are de-duplicated so the shell's URLSearchParams
    // never sees an ambiguous double value.
    let mut params: Vec<(String, String)> = Vec::new();
    for pair in spec_query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        set_param(&mut params, k, v.to_string()); // already encoded - kept verbatim
    }

    let mut output: Option<String> = None;
    let mut format: Option<String> = None;
    let mut i = 0;
    while i < flags.len() {
        let f = &flags[i];
        if !f.starts_with('-') {
            i += 1;
            continue; // stray positional - ignore
        }
        let body = f.trim_start_matches('-');
        let (key, inline) = match body.split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (body, None),
        };
        // Resolve the value: inline `--k=v`, else the next arg if it isn't a flag,
        // else treat as a boolean `--k` (⇒ "1").
        let value = match inline {
            Some(v) => v,
            None => {
                if i + 1 < flags.len() && !flags[i + 1].starts_with('-') {
                    i += 1;
                    flags[i].clone()
                } else {
                    "1".to_string()
                }
            }
        };
        match key {
            "o" | "output" => output = Some(value),
            "f" | "format" | "export" | "e" => format = Some(value),
            _ => set_param(&mut params, key, pct(&value)),
        }
        i += 1;
    }

    let stdout = output.as_deref() == Some("-");

    // Serialize the query. `export=1` is the auto-export trigger; `format=` selects
    // which of the tool's formats (absent ⇒ the tool's first format).
    if let Some(fmt) = &format {
        set_param(&mut params, "format", pct(fmt));
    }
    set_param(&mut params, "export", "1".to_string());
    let query = params
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");

    Mode::Cli(CliJob { tool_id, query, output, stdout })
}

/// Pull the tool id and any query out of a spec that may be a bare id
/// (`qr-code`), an `id?query`, or a full link (`https://…/#/tool/qr-code?url=…`,
/// or the `/t/<id>` short form).
fn parse_spec(spec: &str) -> (String, String) {
    let tail = if let Some(i) = spec.find("/tool/") {
        &spec[i + "/tool/".len()..]
    } else if let Some(i) = spec.find("/t/") {
        &spec[i + "/t/".len()..]
    } else {
        spec
    };
    let (id, query) = tail.split_once('?').unwrap_or((tail, ""));
    (id.trim_matches('/').to_string(), query.to_string())
}

/// Insert-or-replace by key, preserving first-seen order.
fn set_param(list: &mut Vec<(String, String)>, key: &str, val: String) {
    if let Some(entry) = list.iter_mut().find(|(k, _)| k == key) {
        entry.1 = val;
    } else {
        list.push((key.to_string(), val));
    }
}

/// Minimal percent-encoding for a query value (RFC 3986 unreserved kept as-is).
fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn print_version() {
    println!("Lolly {}", env!("CARGO_PKG_VERSION"));
}

pub fn print_help() {
    println!(
        r#"Lolly - the desktop app, run as a command line.

Usage:
  Lolly                                 launch the desktop app (GUI)
  Lolly <tool-id> [--flags]             render a tool headlessly and exit
  Lolly run <tool-id> [--flags]         the same, spelled out
  Lolly <https://lolly.tools/#/tool/…>  render a pasted link (later --flags win)

Options:
  -o, --output <path>   write the export here (`-` for stdout).
                        Absent ⇒ <tool-id>.<ext> in the current directory.
  -f, --format <fmt>    export format (png, svg, pdf, …). Absent ⇒ the tool's first.
      --<key>=<value>   any tool input or reserved URL param (url, width, dpi, c2pa, …).
  -h, --help            this help.
  -V, --version         version.

Examples:
  Lolly qr-code --url=https://suse.com --output=qr.png
  Lolly qr-code --url=https://suse.com --format=svg -o -   > qr.svg
  Lolly color-palette --format=svg --output=palette.svg

Environment:
  LOLLY_CLI_TIMEOUT=<seconds>   render watchdog (default 90).

This is the subset that ships inside the app. The full terminal experience -
`list`, `describe`, `batch`, `preflight`, `validate`, every format tier - is the
Node CLI: run `npm run cli` in the repo, or install the `lolly` package."#
    );
}

/// Run one headless render job, then exit. Never returns.
pub fn run_cli(mut context: tauri::Context, job: CliJob) {
    // Clear the window declared in tauri.conf.json so nothing visible is ever
    // created - we build our own off-screen window in `setup` instead.
    context.config_mut().app.windows.clear();

    let init = build_init_script(&job);

    // Watchdog: a stuck or failed render (e.g. an unmet capability that never
    // resolves) must not hang the terminal. Diagnostics from the page already
    // reach stderr via cli_log; this is the hard stop.
    let secs = watchdog_secs();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(secs));
        eprintln!("lolly: timed out after {secs}s (no output produced). Set LOLLY_CLI_TIMEOUT to raise it.");
        std::process::exit(2);
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_http::init())
        .manage(job)
        // The capture commands read a shared CaptureSession from state; a headless CLI
        // render never signs in, so it stays empty and captures fall back to a fresh
        // headless browser on the persistent profile (which a prior GUI sign-in fills).
        .manage(crate::capture::CaptureSession::default())
        .invoke_handler(tauri::generate_handler![
            crate::capture::capture_page,
            crate::capture::capture_page_pdf,
            cli_write,
            cli_done,
            cli_fail,
            cli_log
        ])
        .setup(move |app| {
            // No dock icon / menu bar for a headless run.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Off-screen but genuinely visible: a hidden/occluded WKWebView throttles
            // rAF, and the tool view's paint is rAF-driven; a visible window ordered
            // far off any display renders normally without ever showing. (The earlier
            // off-screen stall was the first-run instance sheet blocking boot, not
            // occlusion - see instance-choice.ts.)
            tauri::WebviewWindowBuilder::new(app, WINDOW_LABEL, tauri::WebviewUrl::App("index.html".into()))
                .title("Lolly")
                .visible(true)
                .focused(false)
                .position(-4000.0, -4000.0)
                .inner_size(1200.0, 800.0)
                .initialization_script(&init)
                .build()?;
            Ok(())
        })
        .run(context)
        .expect("error while running Lolly desktop (cli)");
}

/// Build the document-start script injected into the headless window: seed the
/// CLI job for the export override, route to the tool, forward diagnostics to
/// stderr, and fail fast on an unknown tool id.
fn build_init_script(job: &CliJob) -> String {
    let global = serde_json::json!({ "output": job.output, "stdout": job.stdout }).to_string();
    let hash = serde_json::to_string(&format!("#/tool/{}?{}", pct(&job.tool_id), job.query)).unwrap();
    let tool_json = serde_json::to_string(&job.tool_id).unwrap();

    let mut s = String::new();
    s.push_str("window.__LOLLY_CLI__ = ");
    s.push_str(&global);
    s.push_str(";\n");
    s.push_str("try{ if(!location.hash){ location.hash = ");
    s.push_str(&hash);
    s.push_str("; } }catch(e){}\n");
    // Lazy internal invoke - __TAURI_INTERNALS__ may not exist the instant an early
    // error fires; the try/catch just drops the diagnostic in that window.
    s.push_str("function __li(c,a){try{return window.__TAURI_INTERNALS__.invoke(c,a);}catch(e){return null;}}\n");
    s.push_str("(function(){var oe=console.error;console.error=function(){__li('cli_log',{level:'error',msg:Array.prototype.map.call(arguments,String).join(' ')});return oe.apply(console,arguments);};})();\n");
    s.push_str("addEventListener('unhandledrejection',function(e){var r=e&&e.reason;__li('cli_log',{level:'error',msg:'unhandledrejection: '+((r&&r.message)||String(r))});});\n");
    s.push_str("addEventListener('error',function(e){__li('cli_log',{level:'error',msg:'error: '+((e&&e.message)||String(e&&e.error||''))});});\n");
    // Unknown-tool guard: the embedded catalog is fetchable from the app origin.
    // Fail immediately rather than waiting out the watchdog on a typo.
    s.push_str("fetch('/catalog/tools/index.json').then(function(r){return r.json();}).then(function(j){var ids=((j&&j.tools)||[]).map(function(t){return t.id;});if(ids.indexOf(");
    s.push_str(&tool_json);
    s.push_str(")<0){__li('cli_fail',{message:'unknown tool \"'+");
    s.push_str(&tool_json);
    s.push_str("+'\". Run `--help`, or `npm run cli` in the repo to list tools.'});}}).catch(function(){});\n");
    s
}

/// Receive the finished export bytes from the web shell's export override and
/// write them where the user asked, then leave the process running for the
/// `cli_done` that follows.
#[tauri::command]
fn cli_write(job: tauri::State<'_, CliJob>, bytes: Vec<u8>, filename: Option<String>) -> Result<(), String> {
    let n = bytes.len();
    if job.stdout {
        let mut out = std::io::stdout();
        out.write_all(&bytes).map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())?;
        eprintln!("lolly: wrote {n} bytes to stdout");
    } else {
        let target = job
            .output
            .clone()
            .or(filename)
            .unwrap_or_else(|| "lolly-export".to_string());
        std::fs::write(&target, &bytes).map_err(|e| format!("could not write {target}: {e}"))?;
        eprintln!("lolly: wrote {n} bytes to {target}");
    }
    Ok(())
}

/// The render finished successfully - exit clean. `std::process::exit` (not
/// `AppHandle::exit`) guarantees the exit code and immediacy; the payload was
/// already flushed by `cli_write`.
#[tauri::command]
fn cli_done() {
    std::process::exit(0);
}

/// The render failed - print the reason to stderr and exit non-zero.
#[tauri::command]
fn cli_fail(message: String) {
    eprintln!("lolly: {message}");
    std::process::exit(1);
}

/// Forward a page-side diagnostic to stderr (console.error, uncaught errors).
#[tauri::command]
fn cli_log(level: String, msg: String) {
    eprintln!("lolly[{level}]: {msg}");
}
