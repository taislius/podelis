-- Sample data for the demo recording (packaging/demo/demo.tape).
CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT, country TEXT);
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    customer_id INTEGER NOT NULL REFERENCES customers (id),
    total NUMERIC NOT NULL,
    placed_at TEXT NOT NULL
);
INSERT INTO customers VALUES
    (1, 'Ona Petraitė', 'ona@example.com', 'LT'),
    (2, 'Vytautas Daukantas', 'vytautas@example.com', 'LT'),
    (3, 'Emma Schmidt', 'emma@example.com', 'DE'),
    (4, 'Lucas Martin', NULL, 'FR'),
    (5, 'Sofia Rossi', 'sofia@example.com', 'IT');
INSERT INTO orders VALUES
    (1, 1, 120.50, '2026-09-01'), (2, 2, 89.90, '2026-09-02'), (3, 1, 42.00, '2026-09-05'),
    (4, 3, 310.00, '2026-09-07'), (5, 5, 18.75, '2026-09-11'), (6, 2, 230.40, '2026-09-14'),
    (7, 4, 67.20, '2026-09-20'), (8, 1, 15.00, '2026-09-28'), (9, 3, 99.99, '2026-10-01');
CREATE INDEX orders_customer ON orders (customer_id);
