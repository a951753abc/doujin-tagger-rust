"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

global.window = { matchMedia: () => ({ matches: false, addEventListener() {} }), setTimeout };
global.document = { addEventListener() {} };

const root = path.resolve(__dirname, "..");
const html = fs.readFileSync(path.join(root, "static", "index.html"), "utf8");
const script = fs.readFileSync(path.join(root, "static", "app.js"), "utf8");
const {
  normalizeShelfConfiguration,
  reorderShelfItem,
  savedViewFilters,
  savedViewShelfParams,
} = require("../static/app.js");

const normalized = normalizeShelfConfiguration({ items: [
  { shelf_type: "event", saved_view_id: null, position: 9, enabled: false, preview_limit: 12 },
  { shelf_type: "recent", saved_view_id: null, position: 2, enabled: true, preview_limit: 3 },
  { shelf_type: "saved_view", saved_view_id: 7, position: 4, enabled: true, preview_limit: 16 },
] });
assert.deepEqual(normalized.items.map((item) => [item.shelf_type, item.position, item.preview_limit]), [
  ["recent", 0, 8],
  ["saved_view", 1, 16],
  ["event", 2, 12],
]);
assert.equal(normalized.items[2].enabled, false);
assert.deepEqual(reorderShelfItem(normalized.items, 2, -1).map((item) => item.shelf_type), ["recent", "event", "saved_view"]);

const params = savedViewShelfParams({
  q: "blue archive", source: "downloads", tag: ["read", "favorite"], missing: ["event", "circle"],
  untagged: true, sort: "title", direction: "asc",
}, 12);
assert.equal(params.get("per_page"), "12");
assert.equal(params.get("q"), "blue archive");
assert.deepEqual(params.getAll("tag"), ["read", "favorite"]);
assert.deepEqual(params.getAll("missing"), ["event", "circle"]);
assert.equal(params.get("sort"), "title");
assert.equal(params.get("direction"), "asc");
assert.deepEqual(savedViewFilters({ missing: ["event", "circle"], tag: ["read", "favorite"] }), {
  missing: ["event", "circle"], tag: ["read", "favorite"],
});

assert.match(html, /id="edit-shelf-composition"/);
assert.match(html, /id="shelf-composition-dialog"/);
assert.match(html, /id="shelf-composition-list"/);
assert.match(html, /優先顯示於智慧書架清單/);
assert.match(script, /\/api\/shelf-configuration/);
assert.match(script, /function renderShelfComposition/);
assert.match(script, /function renderConfiguredShelf/);
assert.match(script, /function initializeShelfScrollShell/);
assert.match(script, /function restoreShelfEditorFocus/);
assert.match(script, /\(sameAction \|\| fallback\)\?\.focus\(\)/);
assert.match(script, /function deleteActiveSavedView/);
assert.match(script, /section\.setAttribute\("aria-labelledby", headingId\)/);
assert.match(script, /`檢視「\$\{descriptor\.title\}」全部收藏`/);

console.log("Shelf composition UI helper and wiring contract passed");
