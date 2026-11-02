-- Two tables joined by a foreign key, for tests that read links:
-- {child}.parent_id → {parent}.id. Each test names its own, so tests on one
-- server don't share them. A table-level FOREIGN KEY: MySQL before 9.0
-- ignores an inline REFERENCES.
DROP TABLE IF EXISTS {child};
DROP TABLE IF EXISTS {parent};
CREATE TABLE {parent} (id INT PRIMARY KEY);
CREATE TABLE {child} (id INT PRIMARY KEY, parent_id INT, FOREIGN KEY (parent_id) REFERENCES {parent} (id));
