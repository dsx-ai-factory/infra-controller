-- Store the operator-facing result of the most recent Site Explorer iteration.
-- Endpoint rows already hold per-endpoint exploration errors; this singleton
-- captures whole-run failures such as missing global credentials or database
-- setup issues that otherwise only appear in nico-api logs.
CREATE TABLE site_explorer_run_status (
    id                              smallint    PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    started_at                      timestamptz NOT NULL,
    finished_at                     timestamptz NOT NULL,
    success                         boolean     NOT NULL,
    error                           text,
    failure_category                text,
    endpoint_explorations           bigint      NOT NULL,
    endpoint_explorations_success   bigint      NOT NULL,
    endpoint_explorations_failed    bigint      NOT NULL,
    last_successful_finished_at     timestamptz,
    last_failed_finished_at         timestamptz,
    CONSTRAINT site_explorer_run_status_finished_after_started
        CHECK (finished_at >= started_at),
    CONSTRAINT site_explorer_run_status_endpoint_explorations_non_negative
        CHECK (endpoint_explorations >= 0),
    CONSTRAINT site_explorer_run_status_endpoint_explorations_success_non_negative
        CHECK (endpoint_explorations_success >= 0),
    CONSTRAINT site_explorer_run_status_endpoint_explorations_failed_non_negative
        CHECK (endpoint_explorations_failed >= 0),
    CONSTRAINT site_explorer_run_status_endpoint_explorations_within_total
        CHECK (endpoint_explorations_success + endpoint_explorations_failed <= endpoint_explorations),
    CONSTRAINT site_explorer_run_status_success_has_no_failure_category
        CHECK (NOT success OR failure_category IS NULL)
);
