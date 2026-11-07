-- Normalize address-derived DNS hostnames consistently with the Rust
-- address_to_hostname helper. IPv6 addresses must use fully expanded,
-- zero-padded hextets joined with dashes.

CREATE OR REPLACE FUNCTION nico_inet_to_dns_hostname(address inet)
RETURNS text
LANGUAGE plpgsql
IMMUTABLE
STRICT
AS $$
DECLARE
    address_text text;
    parts text[];
    left_groups text[] := ARRAY[]::text[];
    right_groups text[] := ARRAY[]::text[];
    groups text[] := ARRAY[]::text[];
    embedded_ipv4_text text;
    embedded_ipv4_parts text[];
    missing_groups integer;
    hostname text;
BEGIN
    IF family(address) = 4 THEN
        RETURN replace(host(address), '.', '-');
    END IF;

    address_text := host(address);
    embedded_ipv4_text := substring(
        address_text FROM '([0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3})$'
    );
    IF embedded_ipv4_text IS NOT NULL THEN
        embedded_ipv4_parts := string_to_array(embedded_ipv4_text, '.');
        address_text := regexp_replace(
            address_text,
            '([0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3})$',
            to_hex(embedded_ipv4_parts[1]::integer * 256 + embedded_ipv4_parts[2]::integer)
                || ':'
                || to_hex(embedded_ipv4_parts[3]::integer * 256 + embedded_ipv4_parts[4]::integer)
        );
    END IF;

    parts := regexp_split_to_array(address_text, '::');

    IF parts[1] IS NOT NULL AND parts[1] != '' THEN
        left_groups := string_to_array(parts[1], ':');
    END IF;

    IF cardinality(parts) > 1 AND parts[2] != '' THEN
        right_groups := string_to_array(parts[2], ':');
    END IF;

    missing_groups := 8 - cardinality(left_groups) - cardinality(right_groups);
    IF missing_groups < 0 THEN
        RAISE EXCEPTION 'invalid IPv6 address expansion for %', address_text;
    END IF;

    groups := left_groups;
    IF missing_groups > 0 THEN
        groups := groups || array_fill('0'::text, ARRAY[missing_groups]);
    END IF;
    groups := groups || right_groups;

    IF cardinality(groups) != 8 THEN
        RAISE EXCEPTION 'invalid IPv6 address expansion for %', address_text;
    END IF;

    SELECT string_agg(lpad(lower(group_text), 4, '0'), '-' ORDER BY group_index)
    INTO hostname
    FROM unnest(groups) WITH ORDINALITY AS expanded(group_text, group_index);

    RETURN hostname;
END;
$$;

CREATE OR REPLACE VIEW dns_records_instance AS
SELECT
    concat(nico_inet_to_dns_hostname(ip_addrs.value::inet), '.', d.name, '.') AS q_name,
    ip_addrs.value::inet AS resource_record,
    COALESCE(
        rt.type_name,
        CASE WHEN family(ip_addrs.value::inet) = 6 THEN 'AAAA' ELSE 'A' END
    )::varchar(10) AS q_type,
    meta.ttl as ttl,
    d.id as domain_id
FROM
    instances i
JOIN
    machine_interfaces mi ON i.machine_id = mi.machine_id
JOIN
    domains d ON mi.domain_id = d.id
CROSS JOIN LATERAL
    jsonb_array_elements(i.network_config::jsonb->'interfaces') AS iface
CROSS JOIN LATERAL
    jsonb_each_text(iface->'ip_addrs') AS ip_addrs
LEFT JOIN
    dns_record_metadata meta ON meta.id = mi.id
LEFT JOIN
    dns_record_types rt ON meta.record_type_id = rt.id
WHERE
    iface->'function_id'->>'type' = 'physical';
