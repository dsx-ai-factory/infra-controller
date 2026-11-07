INSERT INTO
    machine_validation_tests (
        test_id,
        name,
        description,
        command,
        args,
        contexts,
        timeout,
        version,
        supported_platforms,
        modified_by,
        verified,
        read_only,
        custom_tags,
        components,
        is_enabled
    )
VALUES
    (
        'forge_NvmeCheck',
        'NvmeCheck',
        'Check if all NVMe drives are writeable',
        '/opt/forge/check-nvme-drives.sh',
        '',
        ARRAY ['Discovery','Cleanup','OnDemand'],
        7200,
        'V1-T1744144753768493',
        ARRAY ['sku_090e_modelname_poweredge_r750','sku_0a6b_modelname_poweredge_r760','7z73cto1ww','7z23cto1ww','920_24387_2540_000','7d9rctolww','7d9actolww','7d9ectolww','7d9ecto1ww','thinksystem_sr675_v3_ovx','7d9rcto1ww','7d9acto1ww','sku_0b73_modelname_poweredge_xe9680','p54903_b21'],
        'User',
        true,
        true,
        ARRAY ['dgxcloud'],
        ARRAY ['Compute'],
        true
    );