"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

global.window = {
  matchMedia: () => ({ matches: false, addEventListener() {} }),
  setTimeout,
};
global.document = { addEventListener() {} };

const root = path.resolve(__dirname, "..");
const html = fs.readFileSync(path.join(root, "static", "index.html"), "utf8");
const script = fs.readFileSync(path.join(root, "static", "app.js"), "utf8");
const {
  buildCollectionPageParams,
  collectionWindowRange,
  normalizeLibraryBatchSize,
  retryApplicationBusy,
} = require("../static/app.js");

function functionSource(name) {
  const match = script.match(new RegExp(
    `(?:async\\s+)?function\\s+${name}\\b[\\s\\S]*?(?=\\n  (?:async\\s+)?function\\s+|\\n  if \\(typeof module)`,
  ));
  assert.ok(match, `missing function ${name}`);
  return match[0];
}

const choices = [24, 48, 96, 144, 192];
choices.forEach((choice) => assert.equal(normalizeLibraryBatchSize(choice), choice));
[undefined, null, 0, 47, 200, 99999, "old-value"].forEach((invalid) => {
  assert.equal(normalizeLibraryBatchSize(invalid), 48, `${String(invalid)} must fall back to 48`);
});

async function verifyBusyRetry() {
  let attempts = 0;
  const settings = await retryApplicationBusy(async () => {
    attempts += 1;
    if (attempts < 3) throw Object.assign(new Error("busy"), { code: "application_busy" });
    return { library_batch_size: 192 };
  }, [0, 0]);
  assert.equal(attempts, 3, "bootstrap must wait through brief application contention");
  assert.equal(settings.library_batch_size, 192);

  await assert.rejects(
    retryApplicationBusy(async () => { throw Object.assign(new Error("offline"), { code: "offline" }); }, [0]),
    /offline/,
    "non-contention failures must not be retried",
  );
}

for (const choice of choices) {
  const first = buildCollectionPageParams(1, choice, "title", "asc", {
    q: "blue",
    tag: ["read", "favorite"],
  });
  const second = buildCollectionPageParams(2, choice, "title", "asc", {
    q: "blue",
    tag: ["read", "favorite"],
  });
  assert.equal(first.get("per_page"), String(choice));
  assert.equal(second.get("per_page"), String(choice));
  assert.equal(second.get("page"), "2");
  assert.equal(second.get("sort"), "title");
  assert.equal(second.get("direction"), "asc");
  assert.deepEqual(second.getAll("tag"), ["read", "favorite"]);
}

for (const columns of [1, 4, 7]) {
  for (const anchor of [0, 5_000, 10_000]) {
    const range = collectionWindowRange(10_001, anchor, columns);
    assert.ok(range.end - range.start <= 384, `window must stay bounded for ${columns} columns`);
    assert.equal(range.start % columns, 0, "window start must align to the active layout columns");
  }
}

assert.match(html, /name="library_batch_size"/);
choices.forEach((choice) => assert.match(html, new RegExp(`<option value="${choice}">${choice} 本</option>`)));

const reset = functionSource("applyLibraryBatchSize");
assert.ok(reset.includes("state.requestNumber += 1"), "changing size must invalidate in-flight pages");
assert.ok(reset.includes("state.libraryLoaded = false"), "changing size must force a fresh first page");
assert.ok(reset.includes("state.libraryRestorePage = 1"), "changing size must not reuse an old page offset");

const init = functionSource("init");
const route = functionSource("routeFromHash");
assert.ok(
  init.includes("state.libraryBatchSizeReady = true") && init.indexOf("state.libraryBatchSizeReady = true") < init.indexOf("routeFromHash()"),
  "startup must publish the persisted batch size before routing",
);
assert.ok(
  route.includes("if (!state.libraryBatchSizeReady) return Promise.resolve(false)"),
  "early hash changes must not load a page with the default before settings are ready",
);

const settingsLoad = functionSource("loadSettingsPage");
assert.equal(
  settingsLoad.match(/retryApplicationBusy/g)?.length,
  4,
  "Settings reads must survive brief startup contention instead of showing a stale first option",
);

const savedViewParams = functionSource("libraryParams");
assert.ok(!savedViewParams.includes("per_page"), "batch preference must not become Saved View or URL membership state");
assert.ok(script.includes("const SHELF_LIMIT = 8"), "Shelf preview count must stay independent");
assert.ok(script.includes("const THUMBNAIL_REQUEST_CONCURRENCY = 4"), "thumbnail concurrency must remain bounded");
assert.ok(script.includes('cover.loading = "lazy"'), "Library covers must remain lazy");

const keyboard = functionSource("moveLibraryFocus");
assert.ok(keyboard.includes("await loadMoreCollections()"), "J navigation must cross a load-more boundary");
assert.ok(keyboard.includes("state.items[firstNewIndex]"), "J navigation must focus the first item in the new batch");

verifyBusyRetry().then(() => {
  console.log("Library batch preference, pagination, windowing, and independence contract passed");
}).catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
