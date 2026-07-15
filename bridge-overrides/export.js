// SPDX-License-Identifier: MPL-2.0
/**
 * Desktop export override.
 *
 * The web export API delivers a finished file with `URL.createObjectURL(blob)` +
 * an `<a download>` click (see shells/web/src/bridge/export.ts `download`). A
 * browser turns that into a download; WKWebView hands the navigation to wry, which
 * CANCELS it outright unless a native download handler is registered:
 *
 *   // wry-0.55.1 src/wkwebview/navigation.rs, navigation_policy()
 *   if should_download {
 *     if has_download_handler { ...Policy::Download } else { ...Policy::Cancel }
 *   }
 *
 * `has_download_handler` is `attributes.download_started_handler.is_some()`, and we
 * register none — so every export was silently cancelled on desktop, the same class
 * of bug as the mobile shell's (which the Android WebView dropped instead).
 *
 * So we wrap the web ExportAPI and replace ONLY `download`/`file` (the delivery
 * verbs) with a real save via tauri-plugin-fs. `render()` and everything else are
 * inherited unchanged — the rasteriser is identical. Files land in the user's real
 * Downloads (a "Lolly" subfolder); the user gets a toast confirming.
 *
 * Unlike mobile — where Downloads is an app-private dir only we write to — macOS
 * BaseDirectory.Download is the user's own shared ~/Downloads. So we de-collide
 * rather than overwrite, matching both browser and wry's native download semantics
 * ("qr.png" → "qr (1).png").
 */
import { createExportAPI as createWebExportAPI } from '../../web/src/bridge/export.ts';
import { writeFile, mkdir, exists, BaseDirectory } from '@tauri-apps/plugin-fs';

const SUBDIR = 'Lolly';
const BASE = { baseDir: BaseDirectory.Download };

// Keep only filesystem-safe characters; never let a tool-supplied name traverse.
const sanitize = (name) => (String(name || 'lolly-export').replace(/[^\w.\- ]+/g, '_') || 'lolly-export');

// Split at the LAST dot so "a.tar.gz" → ["a.tar", ".gz"] and a dotfile keeps its
// leading dot as part of the stem (".env" → [".env", ""], never ["", ".env"]).
function splitExt(name) {
  const i = name.lastIndexOf('.');
  return i > 0 ? [name.slice(0, i), name.slice(i)] : [name, ''];
}

/**
 * First free "name (n).ext" in Downloads/Lolly, browser-style. Bounded: after
 * MAX_TRIES we fall through to the plain name and let it overwrite rather than
 * loop forever on a pathological directory.
 */
async function freeName(name) {
  const MAX_TRIES = 100;
  const [stem, ext] = splitExt(name);
  for (let n = 0; n < MAX_TRIES; n++) {
    const candidate = n === 0 ? name : `${stem} (${n})${ext}`;
    if (!(await exists(`${SUBDIR}/${candidate}`, BASE))) return candidate;
  }
  return name;
}

function toast(message, isError) {
  try {
    const t = document.createElement('div');
    t.textContent = message;
    t.style.cssText =
      'position:fixed;left:50%;bottom:24px;transform:translateX(-50%);' +
      'z-index:2147483647;padding:12px 18px;border-radius:12px;max-width:90vw;text-align:center;' +
      'font:14px/1.35 system-ui,-apple-system,sans-serif;box-shadow:0 8px 30px rgba(0,0,0,.35);' +
      (isError ? 'background:#7a1f1f;color:#fff' : 'background:#0c322c;color:#eafff4');
    document.body.appendChild(t);
    setTimeout(() => { t.style.transition = 'opacity .3s'; t.style.opacity = '0'; setTimeout(() => t.remove(), 320); }, 2800);
  } catch { /* no DOM — nothing to show */ }
}

async function saveToDownloads(blob, filename, host) {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  const name = sanitize(filename);
  try {
    if (!(await exists(SUBDIR, BASE))) {
      await mkdir(SUBDIR, { ...BASE, recursive: true });
    }
    const finalName = await freeName(name);
    await writeFile(`${SUBDIR}/${finalName}`, bytes, BASE);
    host?.log?.('info', `Saved ${finalName} to Downloads/${SUBDIR}`);
    toast(`Saved “${finalName}” to Downloads/${SUBDIR}`);
  } catch (err) {
    host?.log?.('error', 'Desktop export save failed', { error: String(err) });
    toast(`Couldn't save “${name}”: ${err?.message || err}`, true);
    throw err;
  }
}

export function createExportAPI(host) {
  const web = createWebExportAPI(host);
  return {
    ...web,
    async download(blob, filename) { await saveToDownloads(blob, filename, host); },
    async file(blob, opts = {}) { await saveToDownloads(blob, opts.filename || 'file', host); },
  };
}
