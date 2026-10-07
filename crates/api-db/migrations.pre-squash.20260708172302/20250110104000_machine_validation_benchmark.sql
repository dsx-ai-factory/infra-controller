UPDATE
    machine_validation_tests
SET
    pre_condition = '/opt/forge/benchpress-cuda-pre-setup.sh',
    extra_output_file = '/opt/benchpress/results/cuda_samples_stdout.txt',
    extra_err_file = '/opt/benchpress/results/cuda_samples_stderr.txt',
    command = '/opt/benchpress/benchpress',
    img_name = null,
    container_arg = null
where
    test_id = 'forge_CudaSample';

UPDATE
    machine_validation_tests
SET
    pre_condition = '/opt/forge/benchpress-raytracing-pre-setup.sh',
    extra_output_file = '/opt/benchpress/results/raytracing_vk_stdout.txt',
    extra_err_file = '/opt/benchpress/results/raytracing_vk_stderr.txt',
    command = '/opt/benchpress/benchpress',
    img_name = null,
    container_arg = null
where
    test_id = 'forge_RaytracingVk';

INSERT INTO
    machine_validation_tests (
        test_id,
        name,
        description,
        img_name,
        container_arg,
        execute_in_host,
        external_config_file,
        command,
        args,
        extra_output_file,
        extra_err_file,
        pre_condition,
        contexts,
        timeout,
        version,
        supported_platforms,
        modified_by,
        verified,
        read_only,
        custom_tags,
        components,
        last_modified_at,
        is_enabled
    )
VALUES
    (
        'forge_Nvbandwidth',
        'Nvbandwidth',
        'Running nvbandwidth test using benechpress',
        NULL,
        NULL,
        false,
        NULL,
        '/opt/benchpress/benchpress',
        'run nvbandwidth',
        '/opt/benchpress/results/nvbandwidth_stdout.txt',
        '/opt/benchpress/results/nvbandwidth_stderr.txt',
        '/opt/forge/benchpress-nvbandwidth-pre-setup.sh',
        '{Discovery,OnDemand}',
        7200,
        'V1-T1734600519831720',
        '{sku_090e_modelname_poweredge_r750,sku_0a6b_modelname_poweredge_r760,7z73cto1ww,7z23cto1ww,920_24387_2540_000}',
        'System',
        true,
        true,
        NULL,
        '{Compute}',
        '2024-12-23 04:25:43.04297+00',
        false
    );

UPDATE
    machine_validation_tests
SET
    is_enabled = false;