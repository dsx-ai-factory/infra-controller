-- This operator-only IPv4 renumbering helper predates multi-prefix admin
-- segments and has no service callers. It created a persistent scratch table
-- at runtime; remove any copy left by its last manual invocation.
DROP PROCEDURE IF EXISTS public.update_admin_network(uuid, inet, inet);

DROP TABLE IF EXISTS public.tmp_network_range_placeholder;
