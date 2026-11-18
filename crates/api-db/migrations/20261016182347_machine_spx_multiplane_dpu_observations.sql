-- Keep each DPU's maintenance readback separate from host desired configuration.
ALTER TABLE machines ADD COLUMN spx_multiplane_status jsonb;
