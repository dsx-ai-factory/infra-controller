BEGIN;

-- Only update rows where `soa` is empty
WITH updated_rows AS (
    SELECT
        id,
        jsonb_set(
            jsonb_set(
                jsonb_set(
                    jsonb_set(
                        jsonb_set(
                            jsonb_set(
                                jsonb_set(
                                    jsonb_set(
                                        soa,
                                        '{primary_ns}', 
                                        to_jsonb('ns1.' || name), -- Dynamically construct "primary_ns"
                                        true
                                    ),
                                    '{contact}', 
                                    to_jsonb('hostmaster.' || name), -- Dynamically construct "contact"
                                    true
                                ),
                                '{serial}', 
                                to_jsonb((to_char(CURRENT_DATE, 'YYYYMMDD') || '01')::BIGINT), -- Dynamically construct "serial"
                                true
                            ),
                            '{refresh}', '3600', true
                        ),
                        '{retry}', '600', true
                    ),
                    '{expire}', '604800', true
                ),
                '{minimum}', '3600', true
            ),
            '{ttl}', '3600', true
        ) AS new_soa
    FROM domains
    WHERE (soa IS NULL OR soa = '{}')
)
UPDATE domains
SET soa = updated_rows.new_soa
FROM updated_rows
WHERE domains.id = updated_rows.id;

COMMIT;

