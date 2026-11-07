-- Views have a problem where they need to be continually dropped/recreated every time a table gets a new column,
-- because if you use e.g. "select m.* from machines m" in a view, the list of columns captured by m.* is decided at
-- view creation time, and does not change when machines gets a new column.  That plus issues with merge conflicts every
-- time the views are touched, make it more trouble than it's worth. We can just inline the query statements in the rust
-- code instead.
DROP VIEW IF EXISTS machine_interface_snapshots CASCADE;
DROP VIEW IF EXISTS machine_snapshots CASCADE;
DROP VIEW IF EXISTS machine_snapshots_with_history CASCADE;
DROP VIEW IF EXISTS network_segment_snapshots CASCADE;
DROP VIEW IF EXISTS network_segment_snapshots_with_history CASCADE;