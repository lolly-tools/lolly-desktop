import { defineConfig } from 'vite';
import { resolve, extname, dirname, join } from 'node:path';
import {
  copyFileSync, existsSync, lstatSync, mkdirSync, readdirSync, readFileSync,
  rmSync, statSync, writeFileSync,
} from 'node:fs';

const webShell  = resolve(__dirname, '../web');
const repoRoot  = resolve(__dirname, '../..');

// The web shell migrated .js → .ts but still references some files by a .js
// specifier (index.html's `/src/main.js` entry; a few `../lib/*.js` imports). The
// web shell's newer rolldown-vite resolves those implicitly; this shell pins an
// older Vite that does not, so map a MISSING .js to its sibling .ts. Only fires
// when the .js is absent and the .ts exists, so it never shadows a real .js.
function jsToTsFallback() {
  return {
    name: 'js-to-ts-fallback',
    enforce: 'pre',
    resolveId(source, importer) {
      if (!source.endsWith('.js')) return null;
      let jsPath;
      if (source.startsWith('/')) jsPath = resolve(webShell, source.slice(1));
      else if (source.startsWith('.') && importer) jsPath = resolve(dirname(importer.split('?')[0]), source);
      else return null; // bare / node_modules specifier — leave alone
      if (existsSync(jsPath)) return null; // a real .js — don't touch it
      const tsPath = jsPath.slice(0, -3) + '.ts';
      return existsSync(tsPath) ? tsPath : null;
    },
  };
}

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js':   'application/javascript',
  '.json': 'application/json',
  '.css':  'text/css',
  '.svg':  'image/svg+xml',
  '.png':  'image/png',
  '.jpg':  'image/jpeg',
  '.jpeg': 'image/jpeg',
  '.webp': 'image/webp',
  '.woff': 'font/woff',
  '.woff2':'font/woff2',
  '.ttf':  'font/ttf',
};

// Embedded content mode (plans/131 WP-A). LOLLY_EMBED_CATALOG selects what
// bundleRepoDirs embeds:
//   'profile' (default) — the active repo-root tools/ + catalog/ views, as ever.
//   'neutral'           — the community/app-store build: the lolly-start TOOLSET
//                         (community ∪ brands/lolly-start/tools, the blank-brand
//                         profile, independent of the ACTIVE view) plus a ~1 MB
//                         neutral catalog seed — the generated tool index, the
//                         asset index filtered to the entries whose bytes ride
//                         along (tokens, palette, demo, songs), and those bytes.
//                         No previews/, no og/, no loops/modules media: brand
//                         content arrives from the instance the user connects
//                         (lib/instance.ts) or a loaded .lolly pack. Also drops
//                         the /info narration audio (plans/131 B.3: Listen moves
//                         to device TTS in the apps; the player resolves null
//                         and mounts nothing until the TTS host lands).
const EMBED_CATALOG = process.env.LOLLY_EMBED_CATALOG ?? 'profile';
if (!['profile', 'neutral'].includes(EMBED_CATALOG)) {
  throw new Error(`LOLLY_EMBED_CATALOG must be 'profile' or 'neutral', got '${EMBED_CATALOG}'`);
}

// Asset-id prefixes the neutral seed EXCLUDES. An id prefix, not a path glob, so
// a future asset added to an excluded family stays excluded. Everything else in
// the lolly-start asset index ships with its bytes.
const NEUTRAL_EXCLUDED_ASSET_PREFIXES = ['lolly/loops/', 'lolly/modules/'];

// fs.cpSync's `dereference: true` does NOT resolve nested directory symlinks
// (verified on Node v24.19.0: copying the tools/ symlink farm reproduces the
// links) — which is how earlier dmgs embedded dist/tools/* as ABSOLUTE symlinks
// into this repo, resolvable only on the build machine. Hand-rolled walk:
// statSync/copyFileSync follow links, so every entry lands as real bytes.
// assertDistState backstops it — a symlink anywhere in dist/ fails the build.
function copyTreeDereferenced(src, dest) {
  if (!statSync(src).isDirectory()) {
    mkdirSync(dirname(dest), { recursive: true });
    copyFileSync(src, dest);
    return;
  }
  mkdirSync(dest, { recursive: true });
  for (const entry of readdirSync(src)) {
    if (entry === '.DS_Store') continue;
    copyTreeDereferenced(join(src, entry), join(dest, entry));
  }
}

/** The blank-brand profile's tool set: community ∪ brands/lolly-start/tools,
 *  later roots winning on id collisions — the same merge scripts/use-profile.ts
 *  performs, minus the overlay (`extends`) machinery, which the lolly-start
 *  profile does not use. A manifest in these roots declaring `extends` fails
 *  the build loudly rather than embedding a partial tool. */
function planNeutralTools() {
  const cfg = JSON.parse(readFileSync(join(repoRoot, 'profiles.json'), 'utf8'));
  const profile = cfg.profiles['lolly-start'];
  if (!profile) throw new Error('profiles.json has no "lolly-start" profile — the neutral build embeds the blank brand');
  const plan = new Map();
  for (const root of profile.tools) {
    const rootAbs = join(repoRoot, root);
    for (const entry of readdirSync(rootAbs)) {
      if (entry.startsWith('.') || entry.startsWith('_') || entry === 'node_modules') continue;
      const src = join(rootAbs, entry);
      if (!statSync(src).isDirectory()) continue;
      let manifest = null;
      try { manifest = JSON.parse(readFileSync(join(src, 'tool.json'), 'utf8')); }
      catch { /* missing/malformed manifest — validate:catalog owns reporting that */ }
      if (manifest && typeof manifest.extends === 'string') {
        throw new Error(
          `${root}/${entry} declares "extends" — the neutral embed has no overlay composer; ` +
          `port scripts/use-profile.ts's composeToolDir before embedding overlay tools`,
        );
      }
      plan.set(entry, src);
    }
  }
  for (const id of profile.exclude ?? []) plan.delete(id);
  return plan;
}

/** The neutral catalog seed: the generated tool index, the filtered asset index,
 *  and exactly the bytes the kept entries reference — resolved from each entry's
 *  format urls, no dir-level heuristics, so the seed can never silently include
 *  an excluded family or reference a file it didn't embed. */
function copyNeutralCatalog(outDir) {
  const brandCatalog = join(repoRoot, 'brands/lolly-start/catalog');
  const destCatalog = join(outDir, 'catalog');
  copyTreeDereferenced(join(brandCatalog, 'tools'), join(destCatalog, 'tools'));
  const index = JSON.parse(readFileSync(join(brandCatalog, 'assets/index.json'), 'utf8'));
  const kept = index.assets.filter((a) => !NEUTRAL_EXCLUDED_ASSET_PREFIXES.some((p) => a.id.startsWith(p)));
  mkdirSync(join(destCatalog, 'assets'), { recursive: true });
  writeFileSync(join(destCatalog, 'assets/index.json'), JSON.stringify({ ...index, assets: kept }, null, 2) + '\n');
  for (const asset of kept) {
    for (const fmt of asset.formats ?? []) {
      if (!fmt.url?.startsWith('/catalog/')) continue;
      const rel = fmt.url.slice('/catalog/'.length);
      copyTreeDereferenced(join(brandCatalog, rel), join(destCatalog, rel));
    }
  }
  for (const doc of ['README.md', 'NOTICE.md']) {
    if (existsSync(join(brandCatalog, doc))) copyFileSync(join(brandCatalog, doc), join(destCatalog, doc));
  }
}

// In dev the Vite dev-server middleware handles /tools/ and /catalog/ requests
// (always against the ACTIVE profile views — the neutral mode is a build
// concern). In production they must be copied into dist/ so the Tauri WebView
// can reach them.
function bundleRepoDirs() {
  return {
    name: 'bundle-repo-dirs',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const url = req.url?.split('?')[0];
        if (!url?.startsWith('/tools/') && !url?.startsWith('/catalog/')) return next();
        const filePath = resolve(repoRoot, url.slice(1));
        if (!existsSync(filePath) || !statSync(filePath).isFile()) return next();
        const data = readFileSync(filePath);
        res.setHeader('Content-Type', MIME[extname(filePath)] ?? 'application/octet-stream');
        res.setHeader('Content-Length', data.byteLength);
        res.end(data);
      });
    },
    writeBundle(options) {
      const outDir = options.dir ?? resolve(__dirname, 'dist');
      if (EMBED_CATALOG === 'profile') {
        for (const dir of ['catalog', 'tools']) {
          copyTreeDereferenced(resolve(repoRoot, dir), resolve(outDir, dir));
        }
        return;
      }
      // neutral: the blank-brand tool set + the seed catalog; the active repo
      // views are not consulted, so the suse profile can stay active locally.
      for (const [id, src] of planNeutralTools()) {
        copyTreeDereferenced(src, join(outDir, 'tools', id));
      }
      copyNeutralCatalog(outDir);
      // Plans/131 B.3: the apps drop the baked Listen narration (~30 MB of
      // .opus that compresses no further). Removing audio-index.json with it
      // makes the player's track resolution return null, so a Listen press
      // no-ops instead of 404ing mid-play.
      rmSync(join(outDir, 'info/audio'), { recursive: true, force: true });
      rmSync(join(outDir, 'info/audio-index.json'), { force: true });
    },
  };
}

// Fail the build unless dist/ is self-contained and shaped as the mode promises.
function assertDistState() {
  const findSymlinks = (dir, out = []) => {
    for (const entry of readdirSync(dir)) {
      const p = join(dir, entry);
      const st = lstatSync(p);
      if (st.isSymbolicLink()) out.push(p);
      else if (st.isDirectory()) findSymlinks(p, out);
    }
    return out;
  };
  return {
    name: 'assert-dist-state',
    writeBundle(options) {
      const outDir = options.dir ?? resolve(__dirname, 'dist');
      const links = findSymlinks(outDir);
      if (links.length) {
        throw new Error(`dist/ contains ${links.length} symlink(s) — the embed would depend on this machine's paths. First: ${links[0]}`);
      }
      const must = (p, why) => {
        if (!existsSync(join(outDir, p))) throw new Error(`dist/${p} missing — ${why}`);
      };
      const mustNot = (p, why) => {
        if (existsSync(join(outDir, p))) throw new Error(`dist/${p} present — ${why}`);
      };
      must('catalog/tools/index.json', 'the embedded tool index');
      must('tools/qr-code/tool.json', 'community tools embed in every mode');
      if (EMBED_CATALOG === 'neutral') {
        mustNot('catalog/previews', 'the neutral seed carries no previews (plans/131 WP-A)');
        mustNot('catalog/og', 'the neutral seed carries no og cards');
        mustNot('catalog/assets/lolly/loops', 'excluded asset family (NEUTRAL_EXCLUDED_ASSET_PREFIXES)');
        mustNot('catalog/assets/lolly/modules', 'excluded asset family (NEUTRAL_EXCLUDED_ASSET_PREFIXES)');
        mustNot('info/audio', 'the apps drop baked narration (plans/131 B.3)');
        mustNot('info/audio-index.json', 'removed with the narration so the Listen player resolves null');
        must('catalog/assets/lolly/tokens/brand.json', 'the neutral brand tokens are the point of the seed');
        const index = JSON.parse(readFileSync(join(outDir, 'catalog/assets/index.json'), 'utf8'));
        for (const asset of index.assets) {
          for (const fmt of asset.formats ?? []) {
            if (fmt.url?.startsWith('/catalog/') && !existsSync(join(outDir, 'catalog', fmt.url.slice('/catalog/'.length)))) {
              throw new Error(`neutral seed: ${asset.id} references ${fmt.url} but the file was not embedded`);
            }
          }
        }
      }
    },
  };
}

// Keep runtime-downloaded assets OUT of the embedded frontend.
//
// The on-device ML models under public/models/ (matte, upscale, kokoro, whisper,
// trustmark — ~1 GB, gitignored + Andy-staged) are fetched at RUNTIME via the
// offline download manager, exactly as the web shell excludes /models/ from its
// app precache group (../web/vite.config.js: the `models` bytes are never in the
// core app). But Vite's publicDir copy pulls the WHOLE public/ tree into dist/,
// and Tauri embeds all of frontendDist into the binary via generate_context!().
// Embedding ~1.8 GB pushes the crate's rlib past the 32-bit `ar` archive-offset
// limit, and the Rust build dies with "truncated or malformed object" (the
// July 164 MB binary never embedded these). So prune them back out after the
// copy: the desktop app downloads them on first use, the same as web.
function pruneEmbeddedDownloads() {
  const RUNTIME_FETCHED = ['models'];
  return {
    name: 'prune-embedded-downloads',
    writeBundle(options) {
      const outDir = options.dir ?? resolve(__dirname, 'dist');
      for (const dir of RUNTIME_FETCHED) {
        rmSync(resolve(outDir, dir), { recursive: true, force: true });
      }
    },
  };
}

// Swap specific web-shell bridge modules for Tauri-native implementations.
// Implemented as a resolveId plugin rather than resolve.alias because the bridge
// imports are RELATIVE siblings ("./capture.js" from bridge/index.js): a path
// regex can't match a relative specifier without also risking same-named files
// elsewhere, so we resolve against the importer and replace only the exact web
// bridge file. (state.js → filesystem state; capture.js → native page capture.)
function overrideBridgeModules(map) {
  return {
    name: 'override-bridge-modules',
    enforce: 'pre',
    resolveId(source, importer) {
      if (!importer) return null;
      // Redirect the web bridge's own sibling imports (./state, ./capture,
      // ./capabilities-provided) to the Tauri versions. Matched by the source's
      // basename + the importer living in a bridge/ dir, so it works for BOTH the
      // absolute fs importer (`vite build`) and the root-relative URL importer the
      // dev server passes (`/src/bridge/index`).
      if (!/[\\/]bridge[\\/]/.test(importer.split('?')[0])) return null;
      // Match on the extension-LESS basename so it holds whether the web bridge
      // imports ./state.js OR ./state.ts. The bridge switched to explicit .ts
      // specifiers (JS→TS migration); keying on '.js' silently missed every
      // override, so the shell shipped web IndexedDB state + a throwing capture stub.
      const name = source.split('?')[0].replace(/^.*[\\/]/, '').replace(/\.[jt]s$/, '');
      return map[name] ?? null;
    },
  };
}

// The desktop shell ships a SMALL bundle (pruneEmbeddedDownloads strips
// dist/models/), so the on-device ML models must be fetched from a model host at
// runtime rather than same-origin. Bake the model host into lib/models-base.ts's
// MODELS_BASE so it applies ONLY to the desktop frontend — the web deploy keeps
// the empty default and self-serves /models/. An override may be passed in the
// environment (VITE_MODELS_BASE).
const MODELS_HOST = process.env.VITE_MODELS_BASE ?? 'https://lolly.tools';

// Inline the model host into MODELS_BASE at its single source (lib/models-base.ts).
// Why a transform and not `define`: Vite's top-level `define` is NOT forwarded to
// the separate WORKER bundles, so the speech workers (which import MODELS_BASE)
// would keep the same-origin '' default and 404 for /models/ on desktop. This
// `enforce: 'pre'` transform runs before Vite's env plugin on the raw source and
// is registered for BOTH the main build (plugins below) and the worker build
// (worker.plugins), so every consumer — main thread and workers alike — inlines
// the same host. Everyone reads MODELS_BASE from this one module, so rewriting it
// here is enough; no other file is touched.
function injectModelsBase(value) {
  return {
    name: 'inject-models-base',
    enforce: 'pre',
    transform(code, id) {
      if (!/[\\/]lib[\\/]models-base\.ts(\?|$)/.test(id)) return null;
      const out = code.replace("import.meta.env?.VITE_MODELS_BASE", JSON.stringify(value));
      if (out === code) throw new Error('inject-models-base: expected VITE_MODELS_BASE read not found in models-base.ts');
      return { code: out, map: null };
    },
  };
}

export default defineConfig({
  root: webShell,
  publicDir: resolve(webShell, 'public'),
  plugins: [
    injectModelsBase(MODELS_HOST),
    jsToTsFallback(),
    overrideBridgeModules({
      'state': resolve(__dirname, 'bridge-overrides/state.ts'),
      'capture': resolve(__dirname, 'bridge-overrides/capture.ts'),
      'capabilities-provided': resolve(__dirname, 'bridge-overrides/capabilities-provided.ts'),
      'export': resolve(__dirname, 'bridge-overrides/export.ts'),
      // Native background removal: routes the wasm-impossible full BiRefNet to a
      // Rust ORT command; other models delegate to the shared wasm runner.
      'matte': resolve(__dirname, 'bridge-overrides/matte.ts'),
      // Native website read for the Design System studio's Website source
      // (plans/97 section 9): a Rust `site_fetch` command, no CSP and no CORS in the
      // way. The web module this replaces is the one WITHOUT a transport — a
      // browser page cannot fetch a third-party origin, so on a plain PWA the
      // studio never renders the tile.
      //
      // THIS KEY MATCHES NOTHING TODAY (checked 2026-08-09). The plugin only
      // rewrites an import made from inside a bridge/ dir, and there is no
      // shells/web/src/bridge/site-fetch.ts to import — so the override never
      // fires. It is left in place for when that module is added; adding it is
      // what turns this back on, and if it is ever renamed this key must follow
      // it (exactly how the '.js' keying above once shipped web IndexedDB
      // state). The failure mode is quiet either way — a missing tile, not a
      // crash — which is why the Website source does NOT depend on it: the web
      // shell probes Tauri's own __TAURI_INTERNALS__.invoke global at runtime
      // (detectSiteTransport in lib/design-system/sources/website.ts) and
      // invokes site_fetch directly. See tauri-shared/bridge-overrides/site-fetch.ts.
      'site-fetch': resolve(__dirname, 'bridge-overrides/site-fetch.ts'),
    }),
    bundleRepoDirs(),
    pruneEmbeddedDownloads(),
    assertDistState(),
  ],
  // Match shells/web/vite.config.js: the web shell renders ZzFXM songs and encodes
  // video in MODULE workers (src/lib/zzfxm-worker.ts, src/bridge/video-encode.worker.ts),
  // and Vite's default worker format is `iife`, which rollup refuses for a
  // code-splitting build. This config does not extend the web one — it rebuilds the
  // options object by hand — so every such setting has to be repeated here, and this
  // one was not: the desktop/mobile FRONTEND build has failed with
  // `Invalid value "iife" for option "output.format"` since the second worker landed
  // (2026-07-20). Keep in sync with the web shell.
  worker: { format: 'es', plugins: () => [injectModelsBase(MODELS_HOST)] },
  // The dev server pre-bundles deps with esbuild, whose default target rejects
  // harfbuzzjs's top-level await (same issue as build.target below). Without this
  // the dev server boots then crashes as soon as a module pulls in harfbuzz.
  optimizeDeps: {
    esbuildOptions: { target: 'esnext' },
  },
  server: {
    port: 5173,
    fs: { allow: [repoRoot] },
  },
  build: {
    outDir: resolve(__dirname, 'dist'),
    emptyOutDir: true,
    // The desktop shell always runs in a modern Tauri WebView (recent Chromium /
    // WebKit), so target esnext. The default (es2020) forbids top-level await,
    // which harfbuzzjs (text-to-path WASM) relies on — without this the frontend
    // build fails in esbuild transpile.
    target: 'esnext',
  },
});
