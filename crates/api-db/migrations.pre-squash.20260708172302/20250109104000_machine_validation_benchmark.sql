UPDATE
    machine_validation_tests
SET
    extra_output_file = '/opt/forge/benchpress/results/cuda_samples_stdout.txt',
    extra_err_file = '/opt/forge/benchpress/results/cuda_samples_stderr.txt'
where
    test_id = 'forge_CudaSample';