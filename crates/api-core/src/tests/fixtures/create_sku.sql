INSERT INTO machine_skus(schema_version, id, description, created, components, device_type)
  VALUES(3, 'sku1','test description',now(),'{"chassis":{"vendor":"vendorA","model":"modelB","architecture":"8806"}, "cpus":[], "gpus":[], "memory":[], "storage":[], "infiniband_devices":[]}'::jsonb, 'device_type');
