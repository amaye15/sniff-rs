-- Revenue per customer, from the shop database.
SELECT c.email, SUM(l.qty * p.price) AS revenue
FROM orders o
JOIN customers c ON c.id = o.customer_id
JOIN order_lines l ON l.order_id = o.id
JOIN products p ON p.sku = l.sku
GROUP BY c.email;
