-- Add the `dpu_bmc_service` credential-rotation family (BF4 DPU BMC `service`
-- account). Isolated in its own migration because Postgres forbids using a newly
-- added enum value in the same transaction that adds it; the seed/backfill that
-- references 'dpu_bmc_service' lives in the following migration.
ALTER TYPE public.credential_rotation_type ADD VALUE 'dpu_bmc_service';
