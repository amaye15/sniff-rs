SELECT region, total FROM region_totals
WHERE region IN (SELECT region FROM clean_orders);
