UPDATE
    machine_validation_tests
SET
    supported_platforms = array_append(supported_platforms, '7dhecto1ww')
WHERE
    test_id IN (
        'forge_DcgmFullLong',
        'forge_DcgmFullShort',
        'forge_MqStresserLong',
        'forge_MqStresserShort',
        'forge_CPUTestLong',
        'forge_CPUTestShort',
        'forge_MemoryTestLong',
        'forge_MemoryTestShort',
        'forge_NvmeCheck'
    )
    AND array_position(supported_platforms, '7dhecto1ww') IS NULL;
