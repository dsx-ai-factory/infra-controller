--- Shoreline
INSERT INTO
    machine_validation_tests (
        args,
        command,
        components,
        container_arg,
        contexts,
        description,
        external_config_file,
        extra_err_file,
        extra_output_file,
        img_name,
        name,
        read_only,
        supported_platforms,
        timeout,
        version,
        test_id,
        custom_tags,
        modified_by
    )
VALUES
    (
        '',
        '',
        ARRAY ['Compute'],
        '--net-host  --env-file /tmp/machine_validation/external_config/shoreline --mount type=bind,src=/opt/shorelineagent/databases,dst=/agent/databases,options=rbind:rw   --mount type=bind,src=/opt/shorelineagent/onprem,dst=/agent/onprem,options=rbind:rw   --mount type=bind,src=/opt/shorelineagent/secrets,dst=/agent/secrets,options=rbind:rw   --mount type=bind,src=/opt/shorelineagent/scraper.yml,dst=/agent/etc/scraper.yml,options=rbind:rw   --mount type=bind,src=/root/.ssh,dst=/agent/.host_ssh,options=rbind:ro   --mount type=bind,src=/opt/shorelineagent/shoreline,dst=/agent/host-etc-shoreline,options=rbind:ro   --memory-limit 524288000   --cpus 0.5',
        ARRAY ['Discovery','Cleanup','OnDemand'],
        'Shoreline Runbook',
        'shoreline',
        '/tmp/error',
        '/tmp/output',
        'docker.io/shorelinesoftware/agent:release-24.0.22-multiarch-lt',
        'ForgeRunBook',
        true,
        ARRAY ['sku_090e_modelname_poweredge_r750','sku_0a6b_modelname_poweredge_r760','7z73cto1ww','7z23cto1ww','default'],
        7200,
        'V1-T1731382251768493',
        'forge_ForgeRunBook',
        ARRAY ['dgxcloud'],
        'User'
    );

--- dcgmi diag
INSERT INTO
    machine_validation_tests (
        args,
        command,
        components,
        contexts,
        description,
        name,
        read_only,
        supported_platforms,
        timeout,
        version,
        test_id,
        pre_condition,
        custom_tags,
        modified_by
    )
VALUES
    (
        'diag -r 3',
        'dcgmi ',
        ARRAY ['GPU'],
        ARRAY ['Discovery','OnDemand'],
        'Full DCGM diag test for 5 min',
        'DcgmFullLong',
        true,
        ARRAY ['sku_090e_modelname_poweredge_r750','sku_0a6b_modelname_poweredge_r760','7z73cto1ww','7z23cto1ww','default'],
        7200,
        'V1-T1731383523746813',
        'forge_DcgmFullLong',
        'nvidia-smi',
        ARRAY ['dgxcloud'],
        'User'
    );

INSERT INTO
    machine_validation_tests (
        args,
        command,
        components,
        contexts,
        description,
        name,
        read_only,
        supported_platforms,
        timeout,
        version,
        test_id,
        pre_condition,
        custom_tags,
        modified_by
    )
VALUES
    (
        'diag -r 2',
        'dcgmi ',
        ARRAY ['GPU'],
        ARRAY ['Cleanup'],
        'Full DCGM diag test for few secs',
        'DcgmFullShort',
        true,
        ARRAY ['sku_090e_modelname_poweredge_r750','sku_0a6b_modelname_poweredge_r760','7z73cto1ww','7z23cto1ww','default'],
        7200,
        'V1-T1731384539962561',
        'forge_DcgmFullShort',
        'nvidia-smi',
        ARRAY ['dgxcloud'],
        'User'
    );

--- stress-ng mq
INSERT INTO
    machine_validation_tests (
        args,
        command,
        components,
        contexts,
        description,
        name,
        read_only,
        supported_platforms,
        timeout,
        version,
        test_id,
        custom_tags,
        modified_by
    )
VALUES
    (
        ' --mq 0 -t 120s --times --perf',
        'stress-ng ',
        ARRAY ['Compute'],
        ARRAY ['Discovery','OnDemand'],
        'stress ng mq stresser for 120',
        'MqStresserLong',
        true,
        ARRAY ['sku_090e_modelname_poweredge_r750','sku_0a6b_modelname_poweredge_r760','7z73cto1ww','7z23cto1ww','default'],
        7200,
        'V1-T1731386879991534',
        'forge_MqStresserLong',
        ARRAY ['dgxcloud'],
        'User'
    );

INSERT INTO
    machine_validation_tests (
        args,
        command,
        components,
        contexts,
        description,
        name,
        read_only,
        supported_platforms,
        timeout,
        version,
        test_id,
        custom_tags,
        modified_by
    )
VALUES
    (
        ' --mq 0 -t 30s --times --perf',
        'stress-ng ',
        ARRAY ['GPU'],
        ARRAY ['Cleanup'],
        'stress ng mq stresser for 30',
        'MqStresserShort',
        true,
        ARRAY ['sku_090e_modelname_poweredge_r750','sku_0a6b_modelname_poweredge_r760','7z73cto1ww','7z23cto1ww','default'],
        7200,
        'V1-T1731386879991534',
        'forge_MqStresserShort',
        ARRAY ['dgxcloud'],
        'User'
    );

--- stress-ng cpu
INSERT INTO
    machine_validation_tests (
        args,
        command,
        components,
        contexts,
        description,
        name,
        read_only,
        supported_platforms,
        timeout,
        version,
        test_id,
        custom_tags,
        modified_by
    )
VALUES
    (
        ' -t 120s --cpu -1 --cpu-method matrixprod',
        'stress-ng ',
        ARRAY ['CPU'],
        ARRAY ['Discovery','OnDemand'],
        'CPU Stress Test 120',
        'CPUTestLong',
        true,
        ARRAY ['sku_090e_modelname_poweredge_r750','sku_0a6b_modelname_poweredge_r760','7z73cto1ww','7z23cto1ww','default'],
        7200,
        'V1-T1731386879991534',
        'forge_CPUTestLong',
        ARRAY ['dgxcloud'],
        'User'
    );

INSERT INTO
    machine_validation_tests (
        args,
        command,
        components,
        contexts,
        description,
        name,
        read_only,
        supported_platforms,
        timeout,
        version,
        test_id,
        custom_tags,
        modified_by
    )
VALUES
    (
        ' -t 30s --cpu -1 --cpu-method matrixprod',
        'stress-ng ',
        ARRAY ['CPU'],
        ARRAY ['Cleanup'],
        'CPU Stress Test 30',
        'CPUTestShort',
        true,
        ARRAY ['sku_090e_modelname_poweredge_r750','sku_0a6b_modelname_poweredge_r760','7z73cto1ww','7z23cto1ww','default'],
        7200,
        'V1-T1731386879991534',
        'forge_CPUTestShort',
        ARRAY ['dgxcloud'],
        'User'
    );

--- stress-ng Memory
INSERT INTO
    machine_validation_tests (
        args,
        command,
        components,
        contexts,
        description,
        name,
        read_only,
        supported_platforms,
        timeout,
        version,
        test_id,
        custom_tags,
        modified_by
    )
VALUES
    (
        ' -t 120s --vm 2 --vm-bytes 50% --vm-keep',
        'stress-ng ',
        ARRAY ['Memory'],
        ARRAY ['Discovery','OnDemand'],
        'Memory Stress Test 120',
        'MemoryTestLong',
        true,
        ARRAY ['sku_090e_modelname_poweredge_r750','sku_0a6b_modelname_poweredge_r760','7z73cto1ww','7z23cto1ww','default'],
        7200,
        'V1-T1731386879991534',
        'forge_MemoryTestLong',
        ARRAY ['dgxcloud'],
        'User'
    );

INSERT INTO
    machine_validation_tests (
        args,
        command,
        components,
        contexts,
        description,
        name,
        read_only,
        supported_platforms,
        timeout,
        version,
        test_id,
        custom_tags,
        modified_by
    )
VALUES
    (
        ' -t 10s --vm 2 --vm-bytes 50% --vm-keep',
        'stress-ng ',
        ARRAY ['Memory'],
        ARRAY ['Cleanup'],
        'Memory Stress Test 10',
        'MemoryTestShort',
        true,
        ARRAY ['sku_090e_modelname_poweredge_r750','sku_0a6b_modelname_poweredge_r760','7z73cto1ww','7z23cto1ww','default'],
        7200,
        'V1-T1731386879991534',
        'forge_MemoryTestShort',
        ARRAY ['dgxcloud'],
        'User'
    );