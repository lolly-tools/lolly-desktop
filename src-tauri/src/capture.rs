//! Page capture — the engine `capture` capability fulfilled natively.
//!
//! The web shell can't screenshot a cross-origin URL (a browser page can't read
//! pixels it doesn't own). The desktop shell can, because it drives a headless
//! Chrome over the DevTools Protocol — capturing with full authority, outside any
//! page sandbox. We deliberately use headless Chrome rather than the app's own
//! WKWebView/WebView2: Tauri v2 has no stable API to screenshot arbitrary content
//! with viewport/scroll control.
//!
//! Two commands, one navigation path:
//!   • capture_page      — raster. Page.captureScreenshot with a DOCUMENT-space
//!                         clip: scroll depth + crop insets + an optional range
//!                         extension (the tall strip a scroll video pans over)
//!                         all resolve into one clip rect.
//!   • capture_page_pdf  — vector. Page.printToPDF under `screen` media
//!                         emulation: a TRUE vector print of the page (text,
//!                         boxes, paths) sized to the viewport width and the
//!                         full page height. The JS bridge converts PDF → SVG
//!                         (the engine's pdf-map/pdf-svg path) and windows it.
//!
//! Clip semantics (probed, Chromium ≥ 120): with captureBeyondViewport: true the
//! clip rect is relative to the DOCUMENT, not the scrolled viewport — so scroll
//! depth must land in clip.y, not in window.scrollTo (an earlier version scrolled
//! and clipped at y=0, which silently framed the page top at every depth). We
//! still scroll to the target first, but only so lazy-loaded content near the
//! framed region has a chance to hydrate before the settle wait.
//!
//! Requires a Chrome/Chromium install. CDP returns both formats base64-encoded;
//! the JS override wraps them in AssetRefs.
//!
//! Note on SSRF: this is a tool the user runs locally, so capturing localhost / a
//! private dev server is a *feature*, not a risk — we only reject non-http(s)
//! schemes (no file://, chrome://). The SSRF hardening belongs to the deferred
//! server-side render service, where an attacker could choose the URL.

use std::sync::Arc;
use std::time::Duration;

use headless_chrome::protocol::cdp::{Emulation, Page};
use headless_chrome::{Browser, LaunchOptions, Tab};
use serde::{Deserialize, Serialize};

/// Ceiling on any captured strip, in CSS px — stays comfortably under Chrome's
/// 16384-px texture limit at dpr 1 and bounds the base64 IPC payload. The range
/// extension shrinks first; the framed viewport itself is never truncated.
const MAX_CLIP_H: f64 = 12000.0;

/// Ceiling on the printed page height, in CSS px. The hard PDF limit is 14400
/// *points* per side; paper_height is CSS-px/96 inches ⇒ px·0.75 points, so
/// 19200 px = 14400 pt is the true maximum single page. Real pages never reach
/// it; beyond it we clamp (and content past the cap is unavailable — signalled
/// by page_height being the clamped value the bridge windows against).
const MAX_PDF_H: f64 = 19200.0;

#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "camelCase", default)]
pub struct CropSpec {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSpec {
    pub url: String,
    pub width: u32,
    /// Viewport/capture height in px. Defaults to a 16:9 box if omitted.
    pub height: Option<u32>,
    /// 0..1 fraction of the scrollable height, or a px offset when > 1.
    pub scroll_depth: Option<f64>,
    /// Extend the capture down to this scroll position (same semantics as
    /// scroll_depth) — the strip a scroll video pans over. ≤ scroll_depth ⇒ none.
    pub range_to: Option<f64>,
    /// Settle time after load (and after scrolling) before the shot.
    pub wait_ms: Option<u64>,
    /// Device pixel ratio — renders the clip at this scale for a crisp raster.
    pub dpr: Option<f64>,
    /// Custom CSS injected before the shot (userstyles-style, additive).
    pub css: Option<String>,
    /// Trim insets, each a 0..0.9 fraction of the framed viewport box.
    pub crop: Option<CropSpec>,
}

/// What the raster command hands back: the shot plus the geometry the tool needs
/// to composite it (the cropped frame box, the pan strip, the page itself).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureResult {
    /// Base64 PNG, as CDP returned it.
    pub data: String,
    /// Captured box in CSS px — crop applied, range extension included.
    pub width: u32,
    pub height: u32,
    /// The cropped viewport height alone (the pan window; height − frameHeight
    /// is the pan distance a scroll clip travels).
    pub frame_height: u32,
    /// Page geometry at capture time, CSS px.
    pub page_width: f64,
    pub page_height: f64,
    /// The resolved scroll offset the frame starts at (document space).
    pub scroll_y: f64,
}

/// What the vector command hands back: a full-page vector PDF + the screen-space
/// geometry the JS bridge windows it with.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorResult {
    /// Base64 PDF, as CDP returned it.
    pub data: String,
    /// Page geometry at capture time, CSS px (the printed height may be capped
    /// at MAX_PDF_H; page_height reports the capped value actually printed).
    pub page_width: f64,
    pub page_height: f64,
}

#[tauri::command]
pub async fn capture_page(spec: CaptureSpec) -> Result<CaptureResult, String> {
    // headless_chrome is blocking; keep it off the async runtime's threads.
    tauri::async_runtime::spawn_blocking(move || capture_blocking(spec))
        .await
        .map_err(|e| format!("capture task panicked: {e}"))?
}

#[tauri::command]
pub async fn capture_page_pdf(spec: CaptureSpec) -> Result<VectorResult, String> {
    tauri::async_runtime::spawn_blocking(move || capture_pdf_blocking(spec))
        .await
        .map_err(|e| format!("capture task panicked: {e}"))?
}

// ── shared navigation path ──────────────────────────────────────────────────────

/// Launch, open, navigate, inject the userstyle. The browser must outlive the tab.
fn open_page(spec: &CaptureSpec, height: u32) -> Result<(Browser, Arc<Tab>), String> {
    if !(spec.url.starts_with("http://") || spec.url.starts_with("https://")) {
        return Err("Only http(s) URLs can be captured.".into());
    }

    let launch = LaunchOptions::default_builder()
        .window_size(Some((spec.width, height)))
        .build()
        .map_err(|e| format!("launch options: {e}"))?;

    let browser = Browser::new(launch).map_err(|e| format!("launch chrome: {e}"))?;
    let tab = browser.new_tab().map_err(|e| format!("new tab: {e}"))?;
    tab.set_default_timeout(Duration::from_secs(30));

    tab.navigate_to(&spec.url)
        .map_err(|e| format!("navigate: {e}"))?;
    tab.wait_until_navigated()
        .map_err(|e| format!("load: {e}"))?;

    // Inject custom CSS as a <style> appended to the document, so it layers over
    // the page's own rules by source order (userstyles-style, additive). Done
    // before scroll/settle so the page reflows and settles with it applied.
    if let Some(css) = spec.css.as_deref() {
        let css = css.trim();
        if !css.is_empty() {
            // serde_json produces a safe, fully-escaped JS string literal.
            let literal = serde_json::to_string(css).unwrap_or_else(|_| "\"\"".into());
            let js = format!(
                "(function(){{var s=document.createElement('style');s.setAttribute('data-lolly-userstyle','');s.textContent={literal};(document.head||document.documentElement).appendChild(s);}})();"
            );
            let _ = tab.evaluate(&js, false);
        }
    }

    Ok((browser, tab))
}

/// Page geometry, measured in the page itself (CSS px).
fn measure(tab: &Tab) -> (f64, f64, f64) {
    let js = "JSON.stringify({pw: document.documentElement.scrollWidth, ph: Math.max(document.body ? document.body.scrollHeight : 0, document.documentElement.scrollHeight), vh: window.innerHeight})";
    let fallback = (0.0, 0.0, 0.0);
    let Ok(obj) = tab.evaluate(js, false) else { return fallback };
    let Some(serde_json::Value::String(s)) = obj.value else { return fallback };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else { return fallback };
    let n = |k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
    (n("pw"), n("ph"), n("vh"))
}

/// A scroll position: 0..1 ⇒ fraction of the scrollable height, > 1 ⇒ px offset.
/// Clamped into the page's real scroll range.
fn resolve_scroll(depth: f64, page_h: f64, viewport_h: f64) -> f64 {
    let max = (page_h - viewport_h).max(0.0);
    let px = if depth <= 1.0 { depth.max(0.0) * max } else { depth };
    px.clamp(0.0, max)
}

fn clamp_inset(v: f64) -> f64 {
    if v.is_finite() { v.clamp(0.0, 0.9) } else { 0.0 }
}

fn capture_blocking(spec: CaptureSpec) -> Result<CaptureResult, String> {
    let vw = spec.width.max(1) as f64;
    let vh = spec.height.unwrap_or((spec.width * 9 / 16).max(1)).max(1) as f64;
    let scale = spec.dpr.filter(|d| *d > 0.0).unwrap_or(1.0);

    let (_browser, tab) = open_page(&spec, vh as u32)?;
    let (pw, ph, real_vh) = measure(&tab);
    // The window was launched at the requested size, but measure the truth (the
    // headless window may quantise) and fall back to the request when blocked.
    let vh = if real_vh > 0.0 { real_vh } else { vh };
    let ph = if ph > 0.0 { ph } else { vh };

    let from = resolve_scroll(spec.scroll_depth.unwrap_or(0.0), ph, vh);
    let extra = spec
        .range_to
        .map(|t| (resolve_scroll(t, ph, vh) - from).max(0.0))
        .unwrap_or(0.0);

    // Scroll to the framed region — NOT for framing (the clip below is document-
    // space), but so lazy-loaded content near it hydrates before the settle.
    if from > 0.0 {
        let _ = tab.evaluate(&format!("window.scrollTo(0, {from});"), false);
    }
    std::thread::sleep(Duration::from_millis(spec.wait_ms.unwrap_or(500)));

    // Crop insets frame a window inside the viewport box; the range extension
    // stretches that window down the page. All document-space.
    let c = spec.crop.unwrap_or_default();
    let (l, r, t, b) = (clamp_inset(c.left), clamp_inset(c.right), clamp_inset(c.top), clamp_inset(c.bottom));
    let frame_w = (vw * (1.0 - l - r)).max(1.0);
    let frame_h = (vh * (1.0 - t - b)).max(1.0);
    // Chrome rejects clips past the page edge; also bound the strip (texture +
    // IPC ceilings — at high dpr the texture limit is the binding one).
    let max_h = MAX_CLIP_H.min(16000.0 / scale);
    let x = vw * l;
    let y = (from + vh * t).min((ph - frame_h).max(0.0));
    let h = (frame_h + extra).min(max_h).min((ph - y).max(frame_h));

    let shot = tab
        .call_method(Page::CaptureScreenshot {
            format: Some(Page::CaptureScreenshotFormatOption::Png),
            quality: None,
            clip: Some(Page::Viewport {
                x,
                y,
                width: frame_w,
                height: h,
                scale,
            }),
            from_surface: Some(true),
            capture_beyond_viewport: Some(true),
            optimize_for_speed: None,
        })
        .map_err(|e| format!("screenshot: {e}"))?;

    Ok(CaptureResult {
        data: shot.data,
        width: frame_w.round() as u32,
        height: h.round() as u32,
        frame_height: frame_h.round().min(h.round()) as u32,
        page_width: pw,
        page_height: ph,
        scroll_y: y,
    })
}

fn capture_pdf_blocking(spec: CaptureSpec) -> Result<VectorResult, String> {
    let vw = spec.width.max(1) as f64;
    let vh = spec.height.unwrap_or((spec.width * 9 / 16).max(1)).max(1);

    let (_browser, tab) = open_page(&spec, vh)?;

    // Print with SCREEN styles — without this, @media print rules (and Chrome's
    // print defaults) restyle the page and the "screenshot" stops looking like
    // the site. Set after load: printToPDF re-lays-out against the emulation.
    let _ = tab.call_method(Emulation::SetEmulatedMedia {
        media: Some("screen".into()),
        features: None,
    });

    // Lazy-load hydration for the whole document: walk the page once, then
    // return to the top so position:fixed chrome prints in its resting place.
    // This walk GROWS pages whose below-the-fold images are loading="lazy" with
    // no reserved size, so we measure AFTER it — measuring before would size the
    // paper to the pre-hydration height and printToPDF would drop the grown tail.
    let _ = tab.evaluate("window.scrollTo(0, document.body ? document.body.scrollHeight : 0);", false);
    std::thread::sleep(Duration::from_millis(150));
    let _ = tab.evaluate("window.scrollTo(0, 0);", false);
    std::thread::sleep(Duration::from_millis(spec.wait_ms.unwrap_or(500)));

    let (pw, ph, _vh) = measure(&tab);
    let ph = if ph > 0.0 { ph.min(MAX_PDF_H) } else { f64::from(vh) };

    // One tall page, paper sized to the viewport width × the full page height
    // (96 CSS px per inch), zero margins, backgrounds on. Vector out.
    let printed = tab
        .call_method(Page::PrintToPDF {
            print_background: Some(true),
            scale: Some(1.0),
            paper_width: Some(vw / 96.0),
            paper_height: Some(ph / 96.0),
            margin_top: Some(0.0),
            margin_bottom: Some(0.0),
            margin_left: Some(0.0),
            margin_right: Some(0.0),
            page_ranges: Some("1".into()),
            ..Default::default()
        })
        .map_err(|e| format!("print to pdf: {e}"))?;

    Ok(VectorResult {
        data: printed.data,
        page_width: pw,
        page_height: ph,
    })
}
