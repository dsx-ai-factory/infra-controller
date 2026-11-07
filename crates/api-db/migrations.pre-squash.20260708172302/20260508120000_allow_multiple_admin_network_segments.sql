-- Admin networks may now be represented by multiple network segments.
-- Prefix exclusion constraints still prevent overlapping managed prefixes,
-- but network_segment_type='admin' is no longer globally unique.
DROP INDEX IF EXISTS only_one_admin_network_segment;

