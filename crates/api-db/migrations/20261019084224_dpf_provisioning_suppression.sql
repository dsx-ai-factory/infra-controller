-- Allow DPF provisioning to pause Site Explorer independently of other workflows.
ALTER TABLE bmc_suppressions DROP CONSTRAINT bmc_suppressions_source_check;
ALTER TABLE bmc_suppressions ADD CONSTRAINT bmc_suppressions_source_check
    CHECK (source IN ('decommissioning', 'bmc_credential_rotation', 'factory_reset_bmc', 'dpf_provisioning'));
