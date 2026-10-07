UPDATE
    machine_validation_tests
SET
    supported_platforms = array_append(supported_platforms, '7d9ecto1ww')
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
        'forge_ForgeRunBook',
        'forge_CpuBenchmarkingFp',
        'forge_CpuBenchmarkingInt',
        'forge_CudaSample',
        'forge_FioPath',
        'forge_FioSSD',
        'forge_FioFile',
        'forge_MmMemBandwidth',
        'forge_MmMemLatency',
        'forge_MmMemPeakBandwidth',
        'forge_Nvbandwidth',
        'forge_RaytracingVk'
    )
    AND array_position(supported_platforms, '7d9ecto1ww') IS NULL;

UPDATE
    machine_validation_tests
SET
    pre_condition = '/opt/forge/benchpress-fio-file-pre-setup.sh',
    extra_output_file = '/opt/benchpress/results/fio_file_stdout.txt',
    extra_err_file = '/opt/benchpress/results/fio_file_stderr.txt',
    command = '/opt/benchpress/benchpress',
    img_name = null,
    container_arg = null
where
    test_id = 'forge_FioFile';