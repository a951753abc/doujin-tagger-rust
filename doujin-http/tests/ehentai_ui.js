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
  ehentaiErrorMessage,
  formatEhentaiDate,
  ehentaiGalleryPath,
  ehentaiRouteHash,
  ehentaiSearchPath,
  ehentaiSourceLabel,
  ehentaiSessionStatusPresentation,
  ehentaiSessionTestMessage,
  ehentaiTorrentFilename,
  normalizeEhentaiCursor,
  normalizeEhentaiSessionFlags,
  rememberEhentaiPageCursors,
  resolveEhentaiPageCursor,
  sortEhentaiTorrents,
} = require("../static/app.js");

function functionSource(name) {
  const match = script.match(new RegExp(
    `(?:async\\s+)?function\\s+${name}\\b[\\s\\S]*?(?=\\n  (?:async\\s+)?function\\s+|\\n  if \\(typeof module)`,
  ));
  assert.ok(match, `missing function ${name}`);
  return match[0];
}

assert.match(html, /href="#ehentai" data-route="ehentai"/, "primary navigation must expose the ExHentai route");
for (const id of [
  "ehentai-view",
  "ehentai-search-form",
  "ehentai-search-input",
  "ehentai-loading",
  "ehentai-error",
  "ehentai-empty",
  "ehentai-results",
  "ehentai-previous",
  "ehentai-next",
  "ehentai-detail",
  "ehentai-open-torrents",
  "ehentai-torrent-dialog",
  "ehentai-current-torrent-list",
  "ehentai-outdated-torrent-list",
  "ehentai-session-form",
  "ehentai-cookie",
  "ehentai-session-status",
  "ehentai-session-test",
  "ehentai-session-clear",
]) {
  assert.ok(html.includes(`id="${id}"`), `missing ExHentai UI #${id}`);
}
assert.match(
  html,
  /id="ehentai-cookie"[^>]*type="password"[^>]*autocomplete="new-password"/,
  "Cookie entry must be a non-rehydrating password field",
);
assert.match(html, /id="ehentai-open-torrents"[^>]*aria-haspopup="dialog"/, "torrent action must expose its dialog contract");
assert.match(html, /公開 E-Hentai/, "the UI must explain the public E-Hentai fallback");

assert.equal(ehentaiSearchPath(" blue archive ", 0), "/api/ehentai/search?q=blue+archive&page=0", "first page must omit cursor");
assert.equal(ehentaiSearchPath(" blue archive ", 2, "98765"), "/api/ehentai/search?q=blue+archive&page=2&cursor=98765");
assert.equal(ehentaiSearchPath(" blue archive ", 1, "prev:98765"), "/api/ehentai/search?q=blue+archive&page=1&cursor=prev%3A98765");
assert.equal(ehentaiSearchPath(" blue archive ", 2, "not-a-cursor"), "/api/ehentai/search?q=blue+archive&page=2", "invalid cursor must not reach the API");
assert.equal(ehentaiRouteHash("blue archive", 0), "#ehentai?q=blue+archive");
assert.equal(ehentaiRouteHash("blue archive", 2, "98765"), "#ehentai?q=blue+archive&page=2&cursor=98765", "reload route must retain its cursor");
assert.equal(normalizeEhentaiCursor(" 00123 "), "00123");
assert.equal(normalizeEhentaiCursor(" prev:00123 "), "prev:00123");
assert.equal(normalizeEhentaiCursor("12x"), null);

const pageCursors = new Map([[0, null]]);
assert.equal(resolveEhentaiPageCursor(0, undefined, pageCursors), null, "first page never needs a cursor");
assert.equal(rememberEhentaiPageCursors(pageCursors, 0, null, "111"), "111", "first response must expose the next-page cursor");
assert.equal(resolveEhentaiPageCursor(1, undefined, pageCursors), "111", "Next must use the recorded response cursor");
assert.equal(ehentaiSearchPath("query", 1, resolveEhentaiPageCursor(1, undefined, pageCursors)), "/api/ehentai/search?q=query&page=1&cursor=111");
rememberEhentaiPageCursors(pageCursors, 1, "111", "222");
assert.equal(resolveEhentaiPageCursor(1, undefined, pageCursors), "111", "Previous must recover the earlier page cursor from history");
assert.equal(resolveEhentaiPageCursor(2, "222", new Map([[0, null]])), "222", "reload must recover the current page cursor from the hash");
const reloadCursors = new Map([[0, null]]);
rememberEhentaiPageCursors(reloadCursors, 2, "222", "333", "prev:250");
assert.equal(resolveEhentaiPageCursor(1, undefined, reloadCursors), "prev:250", "reload response must restore a backward cursor for Previous");
assert.equal(resolveEhentaiPageCursor(2, null, pageCursors), null, "an explicitly missing Next cursor must never fall back to page alone");
assert.equal(ehentaiGalleryPath(123, "a/b"), "/api/ehentai/galleries/123/a%2Fb");
assert.equal(ehentaiGalleryPath(123, "token", "/torrents"), "/api/ehentai/galleries/123/token/torrents");
assert.equal(ehentaiSourceLabel("exhentai"), "ExHentai");
assert.equal(ehentaiSourceLabel("ehentai_public_fallback"), "公開 E-Hentai fallback");
assert.equal(ehentaiTorrentFilename('bad:name/part'), "bad_name_part.torrent");

const sorted = sortEhentaiTorrents([
  { name: "older", posted_at: "1735689600" },
  { name: "newest", posted_at: "2025-06-01T00:00:00Z" },
]);
assert.deepEqual(sorted.map((item) => item.name), ["newest", "older"], "torrent rows must be newest first");
assert.equal(
  formatEhentaiDate("1710000000"),
  formatEhentaiDate("2024-03-09T16:00:00.000Z"),
  "gdata Unix seconds must render as the same instant as ISO time",
);
assert.equal(
  formatEhentaiDate("2024-03-10 12:34:56"),
  formatEhentaiDate("2024-03-10T12:34:56"),
  "HTML date/time strings must be normalized before formatting",
);
assert.equal(formatEhentaiDate(null), "時間不明");
assert.equal(formatEhentaiDate(""), "時間不明");
assert.ok(!script.includes("formatDate("), "ExHentai renderers must not call the nonexistent formatDate helper");

const flags = normalizeEhentaiSessionFlags({
  configured: true,
  environment_overridden: true,
  session_valid: true,
  cookie: "must-never-be-rehydrated",
});
assert.deepEqual(flags, { configured: true, override: true, session: "exhentai" });
assert.ok(!JSON.stringify(flags).includes("must-never-be-rehydrated"), "normalized session state must discard raw Cookie data");

const sessionLabels = {
  exhentai: /ExH 可存取/,
  ehentai_only: /僅 E-H/,
  not_configured: /未設定/,
  invalid_cookie: /Cookie 無效/,
  exhentai_unavailable: /Sad Panda/,
  sad_panda: /Sad Panda/,
  rate_limited: /流量限制/,
  network_error: /網路錯誤/,
  parse_error: /解析失敗/,
};
for (const [status, expectedLabel] of Object.entries(sessionLabels)) {
  assert.equal(normalizeEhentaiSessionFlags({ session: status }).session, status, `${status} must survive normalization`);
  assert.match(ehentaiSessionStatusPresentation(status).label, expectedLabel, `${status} needs a distinct status label`);
  if (status !== "exhentai") {
    assert.doesNotMatch(ehentaiSessionTestMessage(status), /測試成功|session 可用/, `${status} must not claim ExHentai success`);
  }
}
assert.equal(normalizeEhentaiSessionFlags({ environment_override: true }).override, true);
assert.equal(normalizeEhentaiSessionFlags({ environment_overridden: true }).override, true);

for (const code of [
  "not_configured",
  "invalid_cookie",
  "exhentai_unavailable",
  "rate_limited",
  "network_error",
  "parse_error",
  "torrent_not_found",
]) {
  const message = ehentaiErrorMessage({ code });
  assert.ok(message.length >= 12, `${code} needs an actionable Traditional Chinese message`);
  assert.doesNotMatch(message, /undefined|null/, `${code} message must be complete`);
}

const searchLoader = functionSource("loadEhentaiSearch");
assert.ok(searchLoader.includes("ehentaiSearchPath(normalizedQuery, normalizedPage, cursor)"), "search must send the resolved cursor");
assert.ok(searchLoader.includes("data.has_next"), "search pagination must render the server has_next flag");
assert.ok(searchLoader.includes("data.next_cursor"), "search pagination must record next_cursor");
assert.ok(searchLoader.includes("data.previous_cursor"), "search pagination must record previous_cursor for reload-safe back navigation");
assert.ok(searchLoader.includes("data.items"), "search must consume gallery items");
assert.ok(functionSource("enterEhentaiRoute").includes('params.has("cursor")'), "hash reload must restore its cursor");
assert.ok(functionSource("prepareEhentaiCursorHistory").includes("new Map([[0, null]])"), "a new query must clear old cursor history");
const searchNavigation = functionSource("navigateEhentaiSearch");
assert.ok(searchNavigation.includes("resolveEhentaiPageCursor"), "page navigation must resolve a cursor before changing routes");
assert.ok(searchNavigation.includes("normalizedPage > 0 && !cursor"), "page > 0 must be blocked without a cursor");
assert.ok(functionSource("renderEhentaiResults").includes("!state.ehentaiNextCursor"), "Next must be disabled without next_cursor");

const galleryLoader = functionSource("loadEhentaiGallery");
assert.ok(galleryLoader.includes("ehentaiGalleryPath(gallery.gid, gallery.token)"), "detail must use gid and token");
const galleryCard = functionSource("renderEhentaiGalleryCard");
for (const field of ["gid", "title", "title_jpn", "category", "thumb", "uploader", "posted_at", "rating", "tags", "pages"]) {
  assert.ok(galleryCard.includes(`gallery.${field}`), `gallery card flow must consume ${field}`);
}

const torrentLoader = functionSource("loadEhentaiTorrents");
assert.ok(torrentLoader.includes('"/torrents"'), "torrent dialog must call the gallery torrent endpoint");
assert.ok(torrentLoader.includes("sortEhentaiTorrents"), "torrent response must be sorted newest first");
const torrentRenderer = `${functionSource("renderEhentaiTorrents")}\n${functionSource("renderEhentaiTorrentItem")}`;
for (const field of ["name", "posted_at", "size", "seeds", "peers", "downloads", "outdated", "torrent_url", "magnet_url"]) {
  assert.ok(torrentRenderer.includes(`torrent.${field}`), `torrent dialog must consume ${field}`);
}

const download = functionSource("downloadEhentaiTorrent");
assert.ok(download.includes('fetch("/api/ehentai/torrents/download"'), "torrent download must use the proxy endpoint");
assert.ok(download.includes("body: JSON.stringify({ url: torrent.torrent_url, name: torrent.name })"), "download proxy needs url and name");
assert.ok(download.includes("new Blob("), "binary download must create a Blob");
assert.ok(download.includes("anchor.download"), "binary download must set a browser filename");

const magnetOpen = functionSource("openEhentaiMagnet");
assert.ok(magnetOpen.includes('api("/api/ehentai/magnets/open"'), "BT handoff must use the local service endpoint");
assert.ok(magnetOpen.includes("body: { magnet_uri: magnetUri }"), "BT handoff must send only magnet_uri");
assert.ok(functionSource("copyEhentaiMagnet").includes("navigator.clipboard"), "magnet copy must wire the clipboard");

const loadSession = functionSource("loadEhentaiSession");
assert.ok(loadSession.includes('api("/api/ehentai/session")'), "settings must load session flags");
assert.ok(loadSession.includes('ui.ehentaiCookie.value = ""'), "status load must clear the password field");
assert.ok(!loadSession.includes("data.cookie"), "status load must never read a Cookie value from the response");
const saveSession = functionSource("saveEhentaiSession");
assert.ok(saveSession.includes('api("/api/ehentai/session", { method: "PUT", body: { cookie } })'), "Cookie save must use PUT {cookie}");
assert.ok(saveSession.includes('ui.ehentaiCookie.value = ""'), "Cookie save must immediately clear the field");
assert.ok(saveSession.includes("Catalog 備用 Cookie 已安全儲存"), "environment override save must explain that only the catalog backup changed");
const clearSession = functionSource("clearEhentaiSession");
assert.ok(clearSession.includes("環境變數 override 仍在使用"), "environment override clear must not claim the effective Cookie was removed");
assert.ok(!saveSession.includes("localStorage"), "Cookie save must never touch localStorage");
assert.ok(!saveSession.includes("toast("), "Cookie save must not place session material in toasts");
assert.ok(functionSource("clearEhentaiSession").includes('method: "DELETE"'), "Cookie clear must use DELETE");
assert.ok(functionSource("testEhentaiSession").includes('api("/api/ehentai/session/test", { method: "POST" })'), "connection test must use POST /session/test");
assert.ok(!script.includes("console.log(cookie"), "Cookie must never be logged");

console.log("ExHentai external source UI contract passed");
