/**
 * Capabilities the Tauri desktop shell fulfils — overrides the web set
 * (shells/web/src/bridge/capabilities-provided.js) at build time via the
 * resolveId override in vite.config.js.
 *
 * Genuine SUPERSET of the web set: the native shell adds page capture (headless
 * Chrome, see bridge-overrides/capture.js) and real filesystem access
 * (tauri-plugin-fs, see bridge-overrides/state.js) ON TOP of everything web
 * provides. It must spread the web list (not re-list it) so a web-side addition
 * — e.g. `compose`, which the desktop shell wires via the shared bridge/index.js
 * — can never silently go missing here and gate that tool off "desktop only" on
 * the desktop itself.
 *
 * The ONE subtraction is 'screen' (engine v1.54): display capture is getDisplayMedia,
 * and wry's webviews don't grant it out of the box — WKWebView needs the host app to
 * answer a display-capture permission delegate, which this shell doesn't implement, so
 * the promise would simply reject. Advertising it would un-grey screencap on desktop
 * and fail at the tap — the exact trap the mobile override documents for 'capture'.
 * Drop this filter once a wry shell actually answers the picker; the bridge code is
 * shared with web and needs no change, only the permission plumbing.
 */
import { PROVIDED_CAPABILITIES as WEB_CAPABILITIES } from '../../web/src/bridge/capabilities-provided.ts';

export const PROVIDED_CAPABILITIES = [...WEB_CAPABILITIES.filter(c => c !== 'screen'), 'filesystem', 'capture'];
