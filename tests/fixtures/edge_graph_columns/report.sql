-- revenue per customer
SELECT o.customer_id, SUM(o.amount) AS total, c.region
FROM orders o
JOIN customers c ON c.cust_id = o.customer_id
WHERE o.placed >= '2024-01-01'
GROUP BY o.customer_id, c.region;
