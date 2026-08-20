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
  batchStatusLabel,
  exportRequest,
  externalBatchNeedsAttention,
  mergeExternalActivityProjection,
} = require("../static/app.js");

function functionSource(name) {
  const match = script.match(new RegExp(
    `(?:async\\s+)?function\\s+${name}\\b[\\s\\S]*?(?=\\n  (?:async\\s+)?function\\s+|\\n  if \\(typeof module)`,
  ));
  assert.ok(match, `missing function ${name}`);
  return match[0];
}

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

assert.ok(script.includes("state.exportJob"), "Activity must track export job state");
assert.ok(script.includes("processed_bytes"), "Activity must render export byte progress");
assert.ok(html.includes("開始後目前不能取消"), "UI must state that v1 cannot cancel");
assert.ok(!/exportRequest\([^)]*destination/i.test(script), "export request must not accept destination path");

const rawFailedJob = {
  id: 41,
  collection_id: 9,
  status: "failed",
  fields: ["authors"],
  error_message: "immutable history",
};
const projectedJob = {
  ...rawFailedJob,
  actionable: false,
  resolution: "acknowledged",
  unresolved_fields: [],
  acknowledged_at: "2026-08-18T00:00:00Z",
};
assert.deepEqual(
  mergeExternalActivityProjection({ ...rawFailedJob, attempts: 2 }, projectedJob),
  { ...projectedJob, attempts: 2 },
  "single-job refresh must preserve the server Activity projection",
);
assert.deepEqual(
  mergeExternalActivityProjection(rawFailedJob),
  {
    ...rawFailedJob,
    actionable: true,
    resolution: null,
    unresolved_fields: ["authors"],
    acknowledged_at: null,
  },
  "a terminal job without an Activity projection must stay conservatively actionable",
);

const failedBatch = {
  items: [{ job_id: 41, status: "failed" }],
  summary: { pending: 0, running: 0, failed: 1, partial: 0 },
};
assert.equal(externalBatchNeedsAttention(failedBatch, new Map(), false), true);
assert.equal(externalBatchNeedsAttention(failedBatch, new Map(), true), false);
assert.equal(
  externalBatchNeedsAttention(failedBatch, new Map([[41, { actionable: true }]]), true),
  true,
);
assert.equal(
  externalBatchNeedsAttention(failedBatch, new Map([[41, { actionable: false }]]), true),
  false,
);
assert.equal(batchStatusLabel(failedBatch.summary, true), "需要檢查");
assert.equal(batchStatusLabel(failedBatch.summary, false), "已完成");

assert.ok(
  functionSource("renderExternalBatch").includes("if (needsAttention) links.append(review)"),
  "resolved batch history must not retain the Review Queue link",
);
assert.ok(
  functionSource("batchSetMetadata").includes("await refreshActivityCenter(true)"),
  "successful batch metadata writes must invalidate Activity immediately",
);
assert.ok(
  functionSource("decideMetadataAssertion").includes("await refreshActivityCenter(true)"),
  "assertion decisions must invalidate Activity immediately",
);
assert.ok(
  functionSource("decideReviewCandidate").includes("await refreshActivityCenter(true)"),
  "Review Queue assertion decisions must invalidate Activity immediately",
);
assert.ok(
  functionSource("scheduleExternalBatchPoll").includes("await refreshExternalSearchActivityProjection()"),
  "a terminal external batch poll must refresh child Activity projections",
);
assert.ok(
  functionSource("loadExternalJob").includes("await refreshExternalSearchActivityProjection()"),
  "a stale single-job 404 must reconcile the server Activity count and items",
);

console.log("export UI, Activity projection and batch lifecycle contracts passed");
