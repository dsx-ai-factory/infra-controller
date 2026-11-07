UPDATE
    machine_validation_tests
SET
    pre_condition = '/opt/forge/benchpress-cuda-pre-setup.sh',
    img_name = '',
    container_arg = ''
where
    test_id = 'forge_CudaSample';