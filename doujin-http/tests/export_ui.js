"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

global.window = {
  matchMedia: () => ({ matches: false, addEventListener() {} }),
};
global.document = { addEventListener() {} };

const root = path.resolve(__dirname, "..");
const html = fs.readFileSync(path.join(root, "static", "index.html"), "utf8");
const script = fs.readFileSync(path.join(root, "static", "app.js"), "utf8");
const { exportRequest, replaceOperationSelection, workBasketHandoffEntries } = require("../static/app.js");

for (const id of [
  "prepare-export",
  "export-dialog",
  "export-root-select",
  "export-package-name",
  "export-preflight-summary",
  "start-export",
  "export-root-list",
  "export-root-form",
]) {
  assert.ok(html.includes(`id="${id}"`), `missing export UI #${id}`);
}

for (const endpoint of [
  "/api/export-roots",
  "/api/export-jobs/preflight",
  "/api/export-jobs/current",
  "/open-location",
]) {
  assert.ok(script.includes(endpoint), `missing export endpoint ${endpoint}`);
}

const selection = new Set([3, 1]);
const request = exportRequest(selection, 7, " C106.zip ");
assert.deepEqual(request, {
  collection_ids: [3, 1],
  export_root_id: 7,
  package_filename: "C106.zip",
});
assert.deepEqual([...selection], [3, 1], "export request must not mutate selection");
assert.equal(request.path, undefined);
assert.equal(request.destination_path, undefined);

const entries = [1, 2, 3].map((id) => ({ collection: { id } }));
const basketSelection = new Set([2]);
const selectedIds = new Set([99]);
const selectedRecords = new Map([[99, { id: 99 }]]);
replaceOperationSelection(
  selectedIds,
  selectedRecords,
  workBasketHandoffEntries(entries, basketSelection),
);
assert.deepEqual(exportRequest(selectedIds, 4, "basket.zip").collection_ids, [2]);
assert.deepEqual([...basketSelection], [2], "Basket handoff must stay copy-only");

assert.ok(script.includes("state.exportJob"), "Activity must track export job state");
assert.ok(script.includes("processed_bytes"), "Activity must render export byte progress");
assert.ok(html.includes("開始後目前不能取消"), "UI must state that v1 cannot cancel");
assert.ok(!/exportRequest\([^)]*destination/i.test(script), "export request must not accept destination path");

console.log("export UI registered-root, selection, preflight and Activity contracts passed");
