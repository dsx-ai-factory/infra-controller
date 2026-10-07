UPDATE
    machine_validation_tests
SET
    pre_condition = '/opt/forge/benchpress-cpu-fp-pre-setup.sh',
    extra_output_file = '/opt/benchpress/results/cpu_2017_fp_rate_light_stdout.txt',
    extra_err_file = '/opt/benchpress/results/cpu_2017_fp_rate_light_stderr.txt',
    command = '/opt/benchpress/benchpress',
    img_name = null,
    container_arg = null
where
    test_id = 'forge_CpuBenchmarkingFp';

UPDATE
    machine_validation_tests
SET
    pre_condition = '/opt/forge/benchpress-cpu-int-pre-setup.sh',
    extra_output_file = '/opt/benchpress/results/cpu_2017_int_rate_light_stderr.txt',
    extra_err_file = '/opt/benchpress/results/cpu_2017_int_rate_light_stdout.txt',
    command = '/opt/benchpress/benchpress',
    img_name = null,
    container_arg = null
where
    test_id = 'forge_CpuBenchmarkingInt';

UPDATE
    machine_validation_tests
SET
    pre_condition = '/opt/forge/benchpress-mem-bandwidth-pre-setup.sh',
    extra_output_file = '/opt/benchpress/results/mm_mem_bandwidth_stderr.txt',
    extra_err_file = '/opt/benchpress/results/mm_mem_bandwidth_stdout.txt',
    command = '/opt/benchpress/benchpress',
    img_name = null,
    container_arg = null
where
    test_id = 'forge_MmMemBandwidth';

UPDATE
    machine_validation_tests
SET
    pre_condition = '/opt/forge/benchpress-mem-peak-bandwidth-pre-setup.sh',
    extra_output_file = '/opt/benchpress/results/mm_mem_peak_bandwidth_stderr.txt',
    extra_err_file = '/opt/benchpress/results/mm_mem_peak_bandwidth_stdout.txt',
    command = '/opt/benchpress/benchpress',
    img_name = null,
    container_arg = null
where
    test_id = 'forge_MmMemPeakBandwidth';

UPDATE
    machine_validation_tests
SET
    pre_condition = '/opt/forge/benchpress-mem-latency-pre-setup.sh',
    extra_output_file = '/opt/benchpress/results/mm_mem_latency_stderr.txt',
    extra_err_file = '/opt/benchpress/results/mm_mem_latency_stdout.txt',
    command = '/opt/benchpress/benchpress',
    img_name = null,
    container_arg = null
where
    test_id = 'forge_MmMemLatency';