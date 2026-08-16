// SPDX-License-Identifier: MPL-2.0
/**
 * Native site transport for the Tauri DESKTOP shell - the platform seam only.
 *
 * Replaces the web shell's `shells/web/src/bridge/site-fetch.ts` at build time
 * via the resolveId override in vite.config.js, exactly as state.ts replaces the
 * IndexedDB state bridge and capture.ts replaces the throwing capture stub. The
 * web module is the one that does NOT have a transport: on a plain PWA the
 * Design System studio never renders the Website tile at all, because a browser
 * page cannot fetch a third-party origin (plans/97 section 9 - connect-src allowlists
 * six hosts, and the decision is that no server fetch is ever built for this).
 *
 * All the logic lives in ../../tauri-shared/bridge-overrides/site-fetch.ts,
 * shared with the mobile shell. What is left here is the `@tauri-apps/api`
 * binding: this shell owns that dependency (the Tauri shells are not npm
 * workspaces, so the parent repo cannot resolve it), and this is where any
 * desktop-only behaviour would go.
 *
 * WIRING, AS OF 2026-08-09: nothing imports this yet. The web shell's Website
 * source finds the native transport through a RUNTIME probe of Tauri's own
 * `__TAURI_INTERNALS__.invoke` global (detectSiteTransport in
 * shells/web/src/lib/design-system/sources/website.ts), which is why the tile
 * appears on this shell at all. The build-time override below needs a
 * `shells/web/src/bridge/site-fetch.ts` to intercept and there is none, so the
 * vite map key matches nothing. This file is the seam for when there is one:
 * its job then is to publish the transport as `host.net._siteFetch`, the
 * optional bridge member website.ts probes before the global.
 *
 * The command it invokes is src-tauri/src/site_fetch.rs, registered in this
 * shell's GUI invoke handler (src-tauri/src/lib.rs).
 */

import { invoke } from '@tauri-apps/api/core';
import {
  createNativeSiteTransport,
  type SiteTransport,
} from '../../tauri-shared/bridge-overrides/site-fetch.ts';

export type { SiteAsset, SiteFetchOptions, SiteFetchResult, SiteTransport } from '../../tauri-shared/bridge-overrides/site-fetch.ts';

/**
 * The shell's site transport. Present means "this shell can read a website",
 * which is what gates the studio's Website tile; it does NOT mean anything has
 * been fetched. Building it is silent and free - the fetch happens only inside
 * `fetchSite`, from the button that states what will be read and by what.
 */
export function createSiteTransport(): SiteTransport {
  return createNativeSiteTransport(invoke);
}

/** Same signature as the shared module's, so a caller that probes rather than
 *  constructs (see nativeSiteTransport there) reads the same answer here. */
export function getSiteTransport(): SiteTransport | null {
  return createSiteTransport();
}
