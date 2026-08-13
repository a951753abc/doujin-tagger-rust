CREATE TABLE work_baskets (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE CHECK (length(trim(name)) > 0),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE work_basket_items (
    basket_id INTEGER NOT NULL REFERENCES work_baskets(id) ON DELETE CASCADE,
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    added_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (basket_id, collection_id)
) STRICT;

CREATE INDEX work_basket_items_order
    ON work_basket_items(basket_id, added_at, collection_id);

INSERT INTO work_baskets(id, name) VALUES (1, '工作籃');
