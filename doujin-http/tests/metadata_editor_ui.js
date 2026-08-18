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
const {
  metadataAuthorsForSave,
  metadataSuggestionPath,
  metadataSuggestionRequestIsCurrent,
  metadataVocabularyField,
} = require("../static/app.js");

function functionSource(name) {
  const match = script.match(new RegExp(
    `(?:async\\s+)?function\\s+${name}\\b[\\s\\S]*?(?=\\n  (?:async\\s+)?function\\s+|\\n  if \\(typeof module)`,
  ));
  assert.ok(match, `missing function ${name}`);
  return match[0];
}

for (const id of [
  "metadata-dialog",
  "metadata-field",
  "metadata-value",
  "metadata-author-chips",
  "metadata-vocabulary-options",
  "metadata-classification-group",
  "metadata-boolean-group",
]) {
  assert.ok(html.includes(`id="${id}"`), `missing metadata editor #${id}`);
}

assert.match(
  html,
  /id="metadata-value"[^>]*role="combobox"[^>]*aria-expanded="false"[^>]*aria-controls="metadata-vocabulary-options"/,
  "metadata value must expose the combobox ARIA contract",
);
assert.match(
  html,
  /id="metadata-vocabulary-options"[^>]*role="listbox"[^>]*hidden/,
  "metadata suggestions must use a hidden listbox",
);
assert.equal(metadataVocabularyField("event"), "event");
assert.equal(metadataVocabularyField("circle"), "circle");
assert.equal(metadataVocabularyField("authors"), "author");
assert.equal(metadataVocabularyField("parody"), "parody");
assert.equal(metadataVocabularyField("title"), null, "title must remain free text");
assert.equal(metadataVocabularyField("classification"), null, "classification must remain structured");
assert.equal(metadataVocabularyField("is_dl"), null, "is_dl must remain structured");

assert.equal(
  metadataSuggestionPath("parody", " Fate/Grand "),
  "/api/vocabulary/suggestions?field=parody&q=Fate%2FGrand&limit=20",
  "suggestions must use the bounded vocabulary endpoint",
);
assert.deepEqual(
  metadataAuthorsForSave(["Alice", " alice ", "Bob"], "Carol,Delta"),
  ["Alice", "Bob", "Carol,Delta"],
  "authors must dedupe while preserving a pending value as one typed author",
);
assert.deepEqual(
  metadataAuthorsForSave(["Alice"], " ALICE "),
  ["Alice"],
  "authors must dedupe existing and pending values case-insensitively",
);

const requestController = { requestNumber: 4, field: "author" };
assert.equal(metadataSuggestionRequestIsCurrent(requestController, 4, "author"), true);
assert.equal(metadataSuggestionRequestIsCurrent(requestController, 3, "author"), false);
assert.equal(metadataSuggestionRequestIsCurrent(requestController, 4, "parody"), false);

const loader = functionSource("loadMetadataSuggestions");
assert.ok(loader.includes("!ui.metadataDialog.open"), "closed dialog responses must be ignored");
assert.ok(loader.includes(".slice(0, 20)"), "the DOM suggestion list must stay capped at 20");
assert.ok(functionSource("initializeMetadataSuggestions").includes("queueMetadataSuggestions(140)"), "typing must debounce requests");
assert.ok(functionSource("syncMetadataEditor").includes("closeMetadataSuggestions()"), "field changes must invalidate old suggestions");
assert.ok(functionSource("closeMetadataSuggestions").includes("controller.requestNumber += 1"), "closing must invalidate in-flight suggestions");

const renderer = functionSource("renderMetadataSuggestions");
assert.ok(renderer.includes("option.aliases"), "canonical options must render alias context when present");
assert.ok(renderer.includes('item.setAttribute("aria-label", option.name)'), "option accessible name must be the canonical name");
assert.ok(renderer.includes('item.setAttribute("aria-describedby"'), "count and alias must be separate descriptions");

const keys = functionSource("handleMetadataSuggestionKeydown");
assert.ok(keys.includes("if (!metadataVocabularyField(ui.metadataField.value)) return"), "title keyboard behavior must stay free-text native");
for (const key of ["ArrowDown", "ArrowUp", "Enter", "Escape", "Tab"]) {
  assert.ok(keys.includes(`"${key}"`), `metadata combobox must handle ${key}`);
}

const save = functionSource("saveMetadata");
assert.ok(save.includes("metadataAuthorsForSave(metadataSuggestionController.authors, ui.metadataValue.value)"), "save must send author chips plus a pending new author");
assert.ok(!save.includes("split(/[、,，"), "authors must not use comma magic");
assert.ok(save.includes("await loadReviewQueue({ preferredId: target.id })"), "Review save must reload with the edited collection preferred");

for (const opener of ["openReviewEditor", "openTriageEditor"]) {
  assert.ok(functionSource(opener).includes("openMetadataDialog("), `${opener} must share the manual metadata dialog`);
}
assert.ok(functionSource("handleKeyboard").includes("const isTyping = target instanceof HTMLInputElement"), "global shortcuts must ignore typing in the combobox input");

console.log("manual metadata editor UI contract passed");
