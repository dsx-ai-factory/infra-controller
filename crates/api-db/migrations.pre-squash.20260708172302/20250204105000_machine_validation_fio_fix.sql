UPDATE
    machine_validation_tests
SET
    pre_condition = '/opt/forge/benchpress-fio-ssd-pre-setup.sh',
    extra_output_file = '/opt/benchpress/results/fio_ssd_stdout.txt',
    extra_err_file = '/opt/benchpress/results/fio_ssd_stderr.txt',
    command = '/opt/benchpress/benchpress',
    img_name = null,
    container_arg = null
where
    test_id = 'forge_FioSSD';

UPDATE
    machine_validation_tests
SET
    pre_condition = '/opt/forge/benchpress-fio-path-pre-setup.sh',
    extra_output_file = '/opt/benchpress/results/fio_path_stdout.txt',
    extra_err_file = '/opt/benchpress/results/fio_path_stderr.txt',
    command = '/opt/benchpress/benchpress',
    args = 'run fio_path --path /tmp/test_fio_path',
    img_name = null,
    container_arg = null
where
    test_id = 'forge_FioPath';