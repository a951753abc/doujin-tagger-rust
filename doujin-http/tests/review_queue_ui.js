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
  claimReviewTerminalJobs,
  createCoalescedReviewQueueRefresh,
  reconcileReviewExternalActivity,
  reviewExternalSearchMode,
  selectReviewExternalJob,
} = require("../static/app.js");

function functionSource(name) {
  const match = script.match(new RegExp(
    `(?:async\\s+)?function\\s+${name}\\b[\\s\\S]*?(?=\\n  (?:async\\s+)?function\\s+|\\n  if \\(typeof module)`,
  ));
  assert.ok(match, `missing function ${name}`);
  return match[0];
}

async function main() {
  assert.match(
    html,
    /id="review-external-status"[^>]*role="status"[^>]*aria-live="polite"[^>]*aria-atomic="true"/,
    "Review job lifecycle must be announced without leaving the Queue",
  );
  assert.ok(html.includes('id="review-search"'), "Review must expose its external-search action");
  assert.ok(html.includes("在品質審核搜尋目前問題欄位；partial 時再試一次"), "shortcut help must document Review W");

  const activeOtherField = { id: 8, collection_id: 3, status: "pending", fields: ["title"] };
  const partialCurrentField = { id: 7, collection_id: 3, status: "partial", fields: ["parody"] };
  assert.equal(
    selectReviewExternalJob(3, "parody", new Map([[3, partialCurrentField]]), new Map([[8, activeOtherField]])),
    activeOtherField,
    "an active collection-level job must win even when its actual fields differ",
  );

  const stalePending = { id: 9, collection_id: 3, status: "pending", fields: ["parody"], updated_at: "old" };
  const freshFailed = { ...stalePending, status: "failed", error_kind: "no_match", updated_at: "new" };
  assert.equal(
    selectReviewExternalJob(3, "parody", new Map([[3, stalePending]]), new Map([[9, freshFailed]])),
    freshFailed,
    "the authoritative Activity snapshot must override a stale Review cache entry",
  );

  assert.equal(
    reviewExternalSearchMode({ type: "missing", field: "parody" }, null, false),
    "checking",
    "an unloaded Activity projection must fail closed before enabling search",
  );
  assert.equal(reviewExternalSearchMode({ type: "missing", field: "parody" }, null, true), "search");
  assert.equal(
    reviewExternalSearchMode(
      { type: "missing", field: "parody" },
      { id: 10, collection_id: 3, status: "succeeded", fields: ["parody"] },
      true,
    ),
    "search",
    "a historical success must not permanently hide search after the field becomes missing again",
  );
  assert.equal(reviewExternalSearchMode({ type: "missing", field: "parody" }, activeOtherField, false), "active");
  assert.equal(reviewExternalSearchMode({ type: "missing", field: "parody" }, partialCurrentField, true), "retry");
  assert.equal(
    reviewExternalSearchMode({ type: "candidate", field: "parody" }, partialCurrentField, true),
    "unavailable",
    "a newly surfaced candidate must stay on the existing Accept/Reject path",
  );
  assert.equal(
    reviewExternalSearchMode({ type: "missing", field: "parody" }, { ...partialCurrentField, status: "failed" }, true),
    "unavailable",
    "failed jobs must not gain a Queue retry bypass",
  );

  const firstTerminal = claimReviewTerminalJobs([freshFailed], new Set());
  assert.deepEqual(firstTerminal.terminalJobs.map((job) => job.id), [9], "a first direct terminal GET must request refresh");
  const duplicateTerminal = claimReviewTerminalJobs([freshFailed], firstTerminal.handledJobIds);
  assert.equal(duplicateTerminal.terminalJobs.length, 0, "the same terminal job must refresh only once");

  const previousJobs = new Map();
  const completedJobs = [];
  for (let id = 1; id <= 100; id += 1) {
    previousJobs.set(id, { id, collection_id: id, status: "pending", fields: ["parody"] });
    completedJobs.push({ id, collection_id: id, status: "succeeded", fields: ["parody"] });
  }
  const reconciliation = reconcileReviewExternalActivity(previousJobs, completedJobs);
  assert.equal(reconciliation.terminalJobs.length, 100, "Activity reconciliation must detect every known active completion");

  let queueReloads = 0;
  const coordinator = createCoalescedReviewQueueRefresh(async () => { queueReloads += 1; });
  const refreshes = reconciliation.terminalJobs.map(() => coordinator.request());
  assert.ok(refreshes.every((promise) => promise === refreshes[0]), "concurrent completions must share one refresh promise");
  await Promise.all(refreshes);
  assert.equal(queueReloads, 1, "100 same-turn completions must coalesce into one Queue reload");

  const enqueue = functionSource("enqueueReviewExternalSearch");
  assert.ok(enqueue.includes("body: { fields: [issue.field] }"), "enqueue must request only the primary Review field");
  assert.ok(enqueue.includes("result.job.fields"), "deduplicated responses must display the actual reused job fields");
  assert.ok(enqueue.includes("refreshActivityCenter(true)"), "enqueue must immediately move Activity monitoring to active cadence");

  const polling = functionSource("loadReviewExternalJob");
  assert.ok(polling.includes("job.collection_id !== collectionId"), "manual job responses must be guarded by Queue collection identity");
  assert.ok(polling.includes("requestReviewTerminalRefresh([job])"), "a first terminal manual GET must reload through the terminal guard");
  assert.ok(!script.includes("reviewExternalJobTimers"), "Review must not retain per-collection polling timers");
  assert.ok(!script.includes("scheduleReviewExternalJobPoll"), "Review must not schedule per-job polling");
  assert.ok(!script.includes("stopReviewExternalJobPoll"), "Review must not own per-job polling cleanup");

  const queueLoad = functionSource("loadReviewQueue");
  assert.ok(
    queueLoad.indexOf("await refreshExternalSearchActivityProjection()") < queueLoad.indexOf("/api/review-queue"),
    "the first Queue snapshot must follow Activity synchronization",
  );
  assert.ok(
    queueLoad.includes("currentReviewItem()?.collection.id ?? preferredId"),
    "page shrink recursion must retain the original collection fallback",
  );

  const activityRefresh = functionSource("refreshExternalSearchActivityProjection");
  assert.ok(activityRefresh.includes("reconcileReviewExternalActivity"), "global Activity refresh must reconcile Review jobs");
  assert.ok(activityRefresh.includes("beforeSignature !== afterSignature"), "unchanged Activity polls must not re-render the live region");
  assert.ok(activityRefresh.includes("requestReviewTerminalRefresh(reconciliation.terminalJobs)"), "global active-to-terminal transitions must reload Queue");

  const terminalReload = functionSource("reloadReviewQueueAfterExternalJobs");
  assert.ok(terminalReload.includes("preserveLiveContext: true"), "terminal refresh must preserve live Queue context");
  assert.ok(!terminalReload.includes("refreshActivityCenter"), "terminal Queue refresh must not recursively poll Activity");

  const keyboard = functionSource("handleKeyboard");
  assert.ok(keyboard.includes('key === "w" && !ui.reviewSearch.disabled && !ui.reviewSearch.hidden'), "W must not repeat an active search");
  assert.ok(keyboard.includes("target?.isContentEditable"), "Review shortcuts must ignore editable targets");
  assert.ok(keyboard.includes("!isDialogOpen()"), "Review shortcuts must not fire inside dialogs");

  console.log("Review Queue external-search reconciliation and lifecycle contract passed");
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
