--- Shoreline
INSERT INTO
    machine_validation_tests (
        args,
        command,
        components,
        container_arg,
        contexts,
        custom_tags,
        description,
        execute_in_host,
        external_config_file,
        extra_err_file,
        extra_output_file,
        img_name,
        name,
        pre_condition,
        supported_platforms,
        timeout,
        version,
        test_id,
        modified_by
    )
VALUES
    (
        'diag -r 3',
        'dcgmi',
        ARRAY ['GPU'],
        '',
        ARRAY ['Discovery','CleanUp','OnDemand'],
        ARRAY ['dgxcloud'],
        'Run run level 3 test cases',
        false,
        '',
        '/tmp/error',
        '/tmp/output',
        '',
        'dcgm_long_test',
        'nvdia-smi',
        ARRAY ['sku_090e_modelname_poweredge_r750','7z73cto1ww'],
        10,
        'V1-T1730906607144616',
        'forge_dcgm_long_test',
        'User'
    );

INSERT INTO
    machine_validation_tests (
        args,
        command,
        components,
        container_arg,
        contexts,
        custom_tags,
        description,
        execute_in_host,
        external_config_file,
        extra_err_file,
        extra_output_file,
        img_name,
        name,
        pre_condition,
        read_only,
        supported_platforms,
        timeout,
        version,
        test_id,
        modified_by
    )
VALUES
    (
        '-f /tmp/shoreline',
        '/bin/agent',
        ARRAY ['Compute'],
        '-v /tmp:/tmp',
        ARRAY ['Discovery','CleanUp'],
        ARRAY ['shoreline'],
        'description1',
        false,
        '/tmp/shoreline',
        '/tmp/error',
        '/tmp/output',
        'nvcr.io/nvidia/shoreline:latest',
        'shoreline_run_book',
        '',
        true,
        ARRAY ['sku_090e_modelname_poweredge_r750','7z73cto1ww'],
        10,
        'V1-T1730906607147909',
        'forge_shoreline_run_book',
        'User'
    );