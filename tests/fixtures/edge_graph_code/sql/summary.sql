-- builds the summary
CREATE TABLE summary AS
SELECT c.name, SUM(s.amount) AS total
FROM sales s JOIN customers c ON s.id = c.id
GROUP BY c.name;
