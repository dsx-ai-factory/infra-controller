-- Migrate from a single IB pkey resource pool to a pkey pool per fabric

UPDATE resource_pool SET name='ib_fabrics.default.pkey' WHERE name='pkey';