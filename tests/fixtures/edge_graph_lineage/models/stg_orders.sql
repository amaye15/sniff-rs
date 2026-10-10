select * from {{ source('shop', 'raw_orders') }}
