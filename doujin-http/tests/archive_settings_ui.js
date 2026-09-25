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

// 設定表單內，商業誌典藏庫欄位位於預設典藏庫之後
const form = html.match(/<form id="settings-form">[\s\S]*?<\/form>/)?.[0];
assert.ok(form, "settings form must exist");
const select = form.match(/<select[^>]*id="commercial-archive-root"[^>]*>/)?.[0];
assert.ok(select, "commercial archive root select must be inside #settings-form");
assert.ok(select.includes('name="commercial_archive_root_id"'), "select must post commercial_archive_root_id");
const defaultIndex = form.indexOf('id="default-archive-root"');
assert.ok(defaultIndex >= 0, "default archive root select must be inside #settings-form");
assert.ok(form.indexOf('id="commercial-archive-root"') > defaultIndex, "commercial select must follow default select");
assert.ok(form.includes('id="commercial-archive-root-note"'), "stale note must exist");
assert.ok(form.includes("商業誌典藏庫"), "field title must exist");
assert.ok(
  form.includes("種類為商業誌的作品歸檔時一律放進這個典藏庫的根目錄，不分場次資料夾；未設定時照場次歸檔。"),
  "field help text must exist",
);

// 三個 PUT /api/settings 呼叫點都要帶上 commercial_archive_root_id
for (const name of ["completeFirstRun", "persistDefaultArchiveRoot", "saveSettings"]) {
  assert.ok(functionSource(name).includes("commercial_archive_root_id"), `${name} must send commercial_archive_root_id`);
}

// 設定頁依已儲存值渲染
assert.ok(functionSource("loadSettingsPage").includes("commercial_archive_root_id"), "settings page must render saved value");

// 失效提示文字
assert.ok(
  script.includes("原設定的商業誌典藏庫已停用或移除，已顯示為「未設定」；再次儲存會清除這項設定。"),
  "stale commercial root message must exist",
);

console.log("archive settings UI checks passed");
