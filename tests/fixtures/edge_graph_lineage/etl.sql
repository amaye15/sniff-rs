CREATE TABLE clean_orders AS
SELECT o.order_id, o.customer_id, o.amount, c.region
FROM raw_orders o JOIN customers c ON c.customer_id = o.customer_id;

INSERT INTO region_totals
SELECT region, SUM(amount) FROM clean_orders GROUP BY region;
