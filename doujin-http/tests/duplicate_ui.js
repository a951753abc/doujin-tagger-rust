"use strict";

const fs = require("node:fs");
const path = require("node:path");

const root = path.resolve(__dirname, "..");
const html = fs.readFileSync(path.join(root, "static", "index.html"), "utf8");
const script = fs.readFileSync(path.join(root, "static", "app.js"), "utf8");

function requireText(source, text, message) {
  if (!source.includes(text)) throw new Error(message);
}

requireText(html, 'data-view="duplicates"', "duplicates must be an independent view");
requireText(html, 'id="duplicate-level"', "duplicates must expose evidence-level filtering");
requireText(html, 'id="duplicate-failures"', "fingerprint failures must be locatable");
requireText(script, 'api(`/api/duplicates${query}`)', "duplicates must use their own candidate API");
requireText(script, 'decision === "exclude" ? "標記不是重複"', "not-duplicate decision is missing");
requireText(script, 'state.selectionContext = "duplicate_delete_handoff"', "delete must use an explicit handoff");
requireText(script, "window.setTimeout(prepareDelete, 0)", "delete must enter the existing confirmation flow");
if (/api\/duplicates\/[^`"']*delete/.test(script)) {
  throw new Error("duplicate detector must not own a delete endpoint");
}
if (script.includes("consolidate_tombstone_candidate(candidate.left")) {
  throw new Error("duplicate review must not reuse tombstone consolidation");
}

console.log("duplicate UI/API separation and delete handoff contract passed");
