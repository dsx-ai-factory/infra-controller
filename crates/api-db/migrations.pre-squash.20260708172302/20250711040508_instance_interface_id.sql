update instances set network_config=jsonb_set(network_config, '{interfaces,0,internal_uuid}', '"66c2ee74-716f-4e90-abd4-a89e0ff20eef"');
