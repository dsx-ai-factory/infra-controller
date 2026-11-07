UPDATE
    machine_validation_tests
SET
    supported_platforms = array_append(supported_platforms, 'thinksystem_sr675_v3_ovx')
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
    AND array_position(supported_platforms, 'thinksystem_sr675_v3_ovx') IS NULL;

UPDATE
    machine_validation_tests
SET
    supported_platforms = array_append(supported_platforms, '7d9rcto1ww')
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
    AND array_position(supported_platforms, '7d9rcto1ww') IS NULL;

UPDATE
    machine_validation_tests
SET
    supported_platforms = array_append(supported_platforms, '7d9acto1ww')
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
    AND array_position(supported_platforms, '7d9acto1ww') IS NULL;

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