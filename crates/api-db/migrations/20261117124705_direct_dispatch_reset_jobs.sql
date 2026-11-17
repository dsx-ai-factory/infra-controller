-- Keep reset references separate from firmware rows owned by older Core writers.
CREATE TABLE direct_dispatch_reset_jobs (
    bmc_mac macaddr NOT NULL,
    job_kind text NOT NULL,
    job_id text NOT NULL,
    created timestamp with time zone DEFAULT now() NOT NULL,
    PRIMARY KEY (bmc_mac, job_kind)
);
