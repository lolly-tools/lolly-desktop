import { defineConfig } from 'vite';
import { resolve, extname, dirname } from 'node:path';
import { existsSync, statSync, readFileSync, cpSync } from 'node:fs';

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

// In dev the Vite dev-server middleware handles /tools/ and /catalog/ requests.
// In production they must be copied into dist/ so the Tauri WebView can reach them.
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
      for (const dir of ['catalog', 'tools']) {
        // dereference: tools/ and catalog are profile VIEWS (symlink farms built
        // by scripts/use-profile.ts) — copy the real files, not the links.
        cpSync(resolve(repoRoot, dir), resolve(outDir, dir), { recursive: true, dereference: true });
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

export default defineConfig({
  root: webShell,
  publicDir: resolve(webShell, 'public'),
  plugins: [
    jsToTsFallback(),
    overrideBridgeModules({
      'state': resolve(__dirname, 'bridge-overrides/state.js'),
      'capture': resolve(__dirname, 'bridge-overrides/capture.js'),
      'capabilities-provided': resolve(__dirname, 'bridge-overrides/capabilities-provided.js'),
      'export': resolve(__dirname, 'bridge-overrides/export.js'),
    }),
    bundleRepoDirs(),
  ],
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
