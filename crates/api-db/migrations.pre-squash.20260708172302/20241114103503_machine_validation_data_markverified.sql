update
    machine_validation_tests
set
    external_config_file = '/tmp/machine_validation/external_config/shoreline'
where
    test_id = 'forge_ForgeRunBook';

update
    machine_validation_tests
set
    verified = true;