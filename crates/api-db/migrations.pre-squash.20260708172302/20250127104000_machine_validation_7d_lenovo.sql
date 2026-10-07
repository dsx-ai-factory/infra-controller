UPDATE
    machine_validation_tests
SET
    supported_platforms = array_append(supported_platforms, '7d9rctolww')
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
    AND array_position(supported_platforms, '7d9rctolww') IS NULL;

UPDATE
    machine_validation_tests
SET
    supported_platforms = array_append(supported_platforms, '7d9actolww')
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
    AND array_position(supported_platforms, '7d9actolww') IS NULL;

UPDATE
    machine_validation_tests
SET
    supported_platforms = array_append(supported_platforms, '7d9ectolww')
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
    AND array_position(supported_platforms, '7d9ectolww') IS NULL;