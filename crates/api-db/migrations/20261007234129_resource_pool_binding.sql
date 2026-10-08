-- Remember which backend owns each pool so editing TOML cannot silently
-- move it to another allocator. Check existing bindings rather than replace them.
CREATE TABLE resource_pool_binding (
    binding_id UUID NOT NULL UNIQUE,
    pool_name TEXT PRIMARY KEY,
    value_domain TEXT NOT NULL CHECK (value_domain IN ('integer', 'ip_address', 'ipv6_prefix')),
    backend_name TEXT,
    backend_kind TEXT NOT NULL CHECK (backend_kind IN ('integrated', 'grpc')),
    authority_id TEXT NOT NULL,
    remote_pool TEXT NOT NULL
);
