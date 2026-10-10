CREATE TABLE audit_log (
  event_id integer PRIMARY KEY,
  actor text,
  action text,
  at timestamp,
  detail text
);
