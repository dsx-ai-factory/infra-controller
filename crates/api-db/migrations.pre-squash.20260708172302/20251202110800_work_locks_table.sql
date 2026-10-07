CREATE TABLE work_locks (
    work_key text PRIMARY KEY,                         -- logical thing we're locking
    worker_id uuid NOT NULL DEFAULT gen_random_uuid(), -- which worker owns it
    started timestamptz NOT NULL DEFAULT now(),        -- when the lock was acquired
    last_keepalive timestamptz NOT NULL DEFAULT now()  -- last time the worker reported as healthy
);

CREATE UNIQUE INDEX idx_work_locks_on_worker_id_and_key ON work_locks(work_key, worker_id);
