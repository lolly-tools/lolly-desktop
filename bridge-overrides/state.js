/**
 * Filesystem-backed state implementation for Tauri shells.
 *
 * Replaces the IndexedDB state bridge (shells/web/src/bridge/state.js) at build
 * time via the resolveId override in vite.config.js. The API surface must stay in
 * sync with that file — tools, the engine, the gallery and catalog sync never see
 * which implementation is running, so a missing method (e.g. sizes) crashes boot.
 *
 * Storage: $APPDATA/Lolly/saved-state/<slot>.json
 */

import { stripAssetModifiers, sessionVersionStamp, migrateSessionRecord, encodeFsToken } from '@lolly/engine';
import {
  BaseDirectory,
  exists,
  mkdir,
  readTextFile,
  writeTextFile,
  readDir,
  remove,
} from '@tauri-apps/plugin-fs';

const STATE_DIR = 'saved-state';
// Written once the legacy-filename migration has completed cleanly (below), so it
// never re-walks the directory on subsequent launches. Not a `.json`, so the
// record readers skip it.
//
// MUST NOT begin with a dot. tauri-plugin-fs defaults require_literal_leading_dot
// to `cfg!(unix)` — true on macOS and Android — so the `$APPDATA/**` glob behind
// fs:scope-appdata-recursive cannot match a dotfile, and every access to one is
// rejected as "forbidden path". As `.slotname-v1` this rejection propagated out
// of ensureMigrated() and failed EVERY state call, so the shell could not boot.
const MIGRATION_MARKER = `${STATE_DIR}/slotname-v1.marker`;

async function ensureDir() {
  const ok = await exists(STATE_DIR, { baseDir: BaseDirectory.AppData });
  if (!ok) {
    await mkdir(STATE_DIR, { baseDir: BaseDirectory.AppData, recursive: true });
  }
}

// Collision-free, cross-platform-safe filename for an arbitrary slot name via
// the engine's reversible percent-encoding codec (encodeFsToken): "Q3 Report",
// "Q3/Report", "Q3+Report", "Q3_Report" and "Björn keynote" all map to DISTINCT
// files — each recoverable to its exact slot. This replaces the old
// `slot.replace(/[^\w.-]/g, '_')`, which collapsed all of those onto one file
// and silently destroyed data (P0-4), and it can't diverge from the mobile
// bridge because both share the one engine codec.
function slotFilename(slot) {
  return `${encodeFsToken(slot)}.json`;
}

function slotPath(slot) {
  return `${STATE_DIR}/${slotFilename(slot)}`;
}

// Where migrateSessionRecord reports a record written by a newer app build.
function stateLog(level, message, meta) {
  (level === 'warn' ? console.warn : console.info)(`[lolly:state] ${message}`, meta ?? '');
}

// One-time migration from the old lossy filename scheme to the collision-free
// one. Old files were named by `slot.replace(/[^\w.-]/g, '_')`, so a session
// named "Q3 Report" lives at `Q3_Report.json` but load() now looks for
// `Q3%20Report.json` and would never find it. Walk saved-state/, read each
// record's authoritative `raw.slot`, and rewrite it under the canonical name.
// (Sessions already lost to a pre-fix collision can't be recovered — only one
// file survived on disk — but the survivor keeps its true name.) Idempotent and
// memoised: a clean pass drops a marker so later launches skip the walk.
let migrationPromise = null;
function ensureMigrated() {
  if (!migrationPromise) migrationPromise = migrateLegacyFilenames();
  return migrationPromise;
}

async function migrateLegacyFilenames() {
  await ensureDir();
  // Defence in depth: a marker read must never fail boot. Every state call awaits
  // this, so a rejection here takes the whole shell down. The walk below is
  // idempotent, so treating an unreadable marker as "not migrated" costs at worst
  // a repeated walk per launch.
  try {
    if (await exists(MIGRATION_MARKER, { baseDir: BaseDirectory.AppData })) return;
  } catch { /* fall through and re-walk */ }

  let entries;
  try {
    entries = await readDir(STATE_DIR, { baseDir: BaseDirectory.AppData });
  } catch {
    return;
  }

  let failures = 0;
  for (const entry of entries) {
    const name = entry.name;
    if (!name?.endsWith('.json')) continue;
    try {
      const text = await readTextFile(`${STATE_DIR}/${name}`, { baseDir: BaseDirectory.AppData });
      const raw = JSON.parse(text);
      const slot = raw?.slot;
      if (typeof slot !== 'string' || !slot) continue;
      const canonical = slotFilename(slot);
      if (canonical === name) continue; // already at its collision-free name

      // Move to the canonical name. If a canonical file already exists (a fresh
      // save under the new scheme), keep whichever is newer so migration never
      // resurrects a stale legacy copy over a real one. Two DIFFERENT slots can
      // no longer map to the same canonical name, so this only fires for a true
      // same-slot duplicate.
      if (await exists(`${STATE_DIR}/${canonical}`, { baseDir: BaseDirectory.AppData })) {
        const targetText = await readTextFile(`${STATE_DIR}/${canonical}`, { baseDir: BaseDirectory.AppData });
        const target = JSON.parse(targetText);
        if ((raw.updatedAt ?? '') > (target?.updatedAt ?? '')) {
          await writeTextFile(`${STATE_DIR}/${canonical}`, text, { baseDir: BaseDirectory.AppData });
        }
      } else {
        await writeTextFile(`${STATE_DIR}/${canonical}`, text, { baseDir: BaseDirectory.AppData });
      }
      await remove(`${STATE_DIR}/${name}`, { baseDir: BaseDirectory.AppData });
    } catch {
      failures++;
    }
  }

  // Only mark done on a fully clean pass; otherwise retry next launch (the walk
  // is idempotent — already-canonical files are skipped instantly).
  if (failures === 0) {
    try {
      await writeTextFile(MIGRATION_MARKER, '1', { baseDir: BaseDirectory.AppData });
    } catch { /* retry next launch */ }
  }
}

// Read every saved record once. Returns { raw, bytes } per file (bytes = the
// on-disk JSON size, matching the web shell's Blob-size estimate). Reused by
// list / sizes / _getAssetRefs so we walk the directory a single way.
async function readAllRecords() {
  await ensureMigrated();
  let entries;
  try {
    entries = await readDir(STATE_DIR, { baseDir: BaseDirectory.AppData });
  } catch {
    return [];
  }
  const out = [];
  for (const entry of entries) {
    if (!entry.name?.endsWith('.json')) continue;
    try {
      const text = await readTextFile(`${STATE_DIR}/${entry.name}`, { baseDir: BaseDirectory.AppData });
      out.push({ raw: JSON.parse(text), bytes: new Blob([text]).size });
    } catch { /* skip corrupt entries */ }
  }
  return out;
}

// createStateAPI signature matches the web shell (db param ignored — not needed here).
export function createStateAPI(_db) {
  return {
    async save(slot, data, thumb = null) {
      await ensureMigrated();
      const record = {
        slot,
        toolId: data.__toolId,
        toolVersion: data.__toolVersion,
        label: data.__label,
        data,
        thumb,
        updatedAt: new Date().toISOString(),
        ...sessionVersionStamp(),
      };
      await writeTextFile(slotPath(slot), JSON.stringify(record, null, 2), {
        baseDir: BaseDirectory.AppData,
      });
    },

    async load(slot) {
      await ensureMigrated();
      const path = slotPath(slot);
      const ok = await exists(path, { baseDir: BaseDirectory.AppData });
      if (!ok) return null;
      try {
        const raw = JSON.parse(await readTextFile(path, { baseDir: BaseDirectory.AppData }));
        // Read version stamps through the shared migrate-or-warn branch rather
        // than reaching for `raw.data` directly (records predating versioning
        // migrate as v0; a newer-app record is read as-is but reported).
        return migrateSessionRecord(raw, stateLog);
      } catch {
        return null;
      }
    },

    async list() {
      const records = await readAllRecords();
      return records
        .map(({ raw }) => ({
          slot: raw.slot,
          toolId: raw.toolId,
          toolVersion: raw.toolVersion,
          label: raw.label,
          filename: raw.data?.__export_filename || null,
          thumb: raw.thumb ?? null,
          updatedAt: raw.updatedAt,
        }))
        .sort((a, b) => (b.updatedAt ?? '').localeCompare(a.updatedAt ?? ''));
    },

    async delete(slot) {
      await ensureMigrated();
      const path = slotPath(slot);
      const ok = await exists(path, { baseDir: BaseDirectory.AppData });
      if (ok) await remove(path, { baseDir: BaseDirectory.AppData });
    },

    async sizes() {
      const result = {};
      for (const { raw, bytes } of await readAllRecords()) {
        if (raw.slot) result[raw.slot] = bytes;
      }
      return result;
    },

    // Blob keys (id:format:version) referenced across all saved sessions, so
    // catalog sync won't evict on-demand blobs a session still needs.
    async _getAssetRefs() {
      const refs = new Set();
      for (const { raw } of await readAllRecords()) collectAssetRefs(raw.data, refs);
      return refs;
    },
  };
}

function collectAssetRefs(value, refs) {
  if (!value || typeof value !== 'object') return;
  if (Array.isArray(value)) {
    for (const item of value) collectAssetRefs(item, refs);
    return;
  }
  if (value.source === 'library' && value.id && value.format && value.version != null) {
    // A modified ref (`<baseId>?theme=<t>` icon OR `<baseId>?treatment=<x>` photo)
    // is derived from the BASE blob — the key the cache holds and pruning must
    // protect. Match the web bridge exactly: strip BOTH modifiers, not just theme,
    // or a saved session's treated photo gets a key that never matches and is evicted.
    const baseId = stripAssetModifiers(String(value.id));
    refs.add(`${baseId}:${value.format}:${value.version}`);
    return;
  }
  for (const v of Object.values(value)) collectAssetRefs(v, refs);
}
