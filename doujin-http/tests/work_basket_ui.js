"use strict";

const assert = require("node:assert/strict");

global.window = {
  matchMedia: () => ({ matches: false, addEventListener() {} }),
};
global.document = {
  addEventListener() {},
};

const {
  replaceOperationSelection,
  workBasketHandoffEntries,
} = require("../static/app.js");

const entries = [1, 2, 3].map((id) => ({ collection: { id, title: `Collection ${id}` } }));
const basketSelection = new Set([2]);
const partial = workBasketHandoffEntries(entries, basketSelection);
assert.deepEqual(partial.map((item) => item.collection.id), [2]);

const previousOperationIds = new Set([99]);
const previousOperationRecords = new Map([[99, { id: 99 }]]);
replaceOperationSelection(previousOperationIds, previousOperationRecords, partial);
assert.deepEqual([...previousOperationIds], [2]);
assert.deepEqual([...previousOperationRecords.keys()], [2]);
assert.deepEqual([...basketSelection], [2], "handoff must not mutate Basket selection");

const all = workBasketHandoffEntries(entries, new Set());
assert.deepEqual(all.map((item) => item.collection.id), [1, 2, 3]);

console.log("work basket UI handoff contract passed");
