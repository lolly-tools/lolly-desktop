/**
 * Filesystem-backed state for the Tauri DESKTOP shell — the platform seam only.
 *
 * Replaces the IndexedDB state bridge (shells/web/src/bridge/state.ts) at build
 * time via the resolveId override in vite.config.js. The API surface must stay in
 * sync with that file — tools, the engine, the gallery and catalog sync never see
 * which implementation is running, so a missing method (e.g. sizes) crashes boot.
 *
 * Storage: $APPDATA/Lolly/saved-state/<slot>.json
 *
 * The logic (slot-name codec, legacy-filename migration, record shape, asset-ref
 * collection) is shared with the mobile shell in ../../tauri-shared/bridge-overrides/state-fs.js;
 * it used to be a byte-identical copy in both shells, so a fix had to be applied
 * twice. All that is left here is the `@tauri-apps/plugin-fs` binding: this shell
 * owns that dependency (the Tauri shells are not npm workspaces, so the parent repo
 * cannot resolve it), and it is where desktop-only storage behaviour would go.
 */

import {
  BaseDirectory,
  exists,
  mkdir,
  readTextFile,
  writeTextFile,
  readDir,
  remove,
} from '@tauri-apps/plugin-fs';
import { createFsStateAPI } from '../../tauri-shared/bridge-overrides/state-fs.js';

// Paths are relative to $APPDATA/Lolly. readDirNames flattens tauri's entry
// objects to names, which is all the shared logic reads.
const appDataFs = {
  exists: (path) => exists(path, { baseDir: BaseDirectory.AppData }),
  mkdirRecursive: (path) => mkdir(path, { baseDir: BaseDirectory.AppData, recursive: true }),
  readTextFile: (path) => readTextFile(path, { baseDir: BaseDirectory.AppData }),
  writeTextFile: (path, text) => writeTextFile(path, text, { baseDir: BaseDirectory.AppData }),
  readDirNames: async (path) =>
    (await readDir(path, { baseDir: BaseDirectory.AppData })).map((entry) => entry.name),
  remove: (path) => remove(path, { baseDir: BaseDirectory.AppData }),
};

// createStateAPI signature matches the web shell (db param ignored — not needed here).
export function createStateAPI(_db) {
  return createFsStateAPI(appDataFs);
}
