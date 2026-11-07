-- 20250818203400_route_server_types.sql
--
-- This adds a new source_type to route_servers, allowing us to independently
-- manage route servers via both config and the admin CLI/API, allowing site
-- operators to choose how they want to manage route servers, without worrying
-- about one type squashing over the other.

-- Create the new route server source type enum type.
CREATE TYPE route_server_source_type AS ENUM ('config_file', 'admin_api');

-- Add the route_servers.source_type column with a default value
-- of config_file, making it so any existing entries just get
-- set as 'config_file' (since they all came in via config file).
ALTER TABLE route_servers
    ADD COLUMN source_type route_server_source_type NOT NULL DEFAULT 'config_file';

-- And now remove the default setting, now that existing rows have
-- been populated with the 'config_file' value.
ALTER TABLE route_servers
    ALTER COLUMN source_type DROP DEFAULT;