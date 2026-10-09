select o.*, c.name from {{ ref('stg_orders') }} o join {{ ref('customers') }} c on o.id = c.id
