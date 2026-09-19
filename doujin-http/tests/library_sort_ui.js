"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const root = path.resolve(__dirname, "..");
const html = fs.readFileSync(path.join(root, "static", "index.html"), "utf8");
const script = fs.readFileSync(path.join(root, "static", "app.js"), "utf8");

function functionSource(name) {
  const match = script.match(new RegExp(
    `(?:async\\s+)?function\\s+${name}\\b[\\s\\S]*?(?=\\n  (?:async\\s+)?function\\s+|\\n  if \\(typeof module)`,
  ));
  assert.ok(match, `missing function ${name}`);
  return match[0];
}

// search-v2-010：排序選單提供檔案大小，遞減在前
const select = html.match(/<select id="library-sort">[\s\S]*?<\/select>/)?.[0];
assert.ok(select, "library sort select must exist");
assert.ok(select.includes('<option value="size:desc">檔案最大</option>'), "sort menu must offer largest-first");
assert.ok(select.includes('<option value="size:asc">檔案最小</option>'), "sort menu must offer smallest-first");
assert.ok(
  select.indexOf('value="size:desc"') < select.indexOf('value="size:asc"'),
  "largest-first must be listed before smallest-first",
);

// 選擇後 state.sort 必須接受 size，而不是退回 created
const change = functionSource("changeLibrarySort");
assert.ok(change.includes('"size"'), "changing the sort menu must accept size");
assert.ok(change.includes("state.libraryFocusId = null"), "changing sort must reload from the first page");

// 由 URL／saved view 還原時也要接受 size
const decode = functionSource("decodeLibraryParams");
assert.ok(decode.includes('"size"'), "hash decoding must accept size");

// 摘要列顯示對應的排序名稱
const summary = functionSource("savedViewSummary");
assert.ok(summary.includes('"size:desc": "檔案最大"'), "summary must label largest-first");
assert.ok(summary.includes('"size:asc": "檔案最小"'), "summary must label smallest-first");

console.log("Library file-size sort menu contract passed");
