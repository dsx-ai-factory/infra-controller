-- Before we do anything, move dns_records over to dns_records_admin_name,
-- since dns_records is a table of mappings based on the pretty name of the
-- administrative IP, and move dns_records_merged into dns_records.
--
-- This could also be done at the end, and we could do a swap of sorts,
-- but this makes the view definition of our new dns_records in line
-- with the ultimate reality.
ALTER VIEW dns_records RENAME TO dns_records_shortname_combined;

--
-- Name: dns_records_adm_combined; Type: TABLE; Schema: public; Owner: carbide_development
--
-- This is a view of DNS records mapping the machine-id -> IP address
-- for both the Hosts and DPUs.
--
-- For example: fm100d12987asd987awe98a.adm.dev3.frg.nvidia.com
--
CREATE VIEW dns_records_adm_combined AS
SELECT
    concat(machine_interfaces.machine_id, '.adm.', domains.name, '.') AS q_name,
    machine_interface_addresses.address AS resource_record
FROM
    machine_interfaces
    JOIN machine_interface_addresses ON (machine_interface_addresses.interface_id = machine_interfaces.id)
    JOIN domains ON ((domains.id = machine_interfaces.domain_id)
            AND (machine_interfaces.primary_interface = TRUE))
WHERE (machine_interfaces.machine_id IS NOT NULL);

--
-- Name: dns_records_bmc_host_id; Type: TABLE; Schema: public; Owner: carbide_development
--
-- This is a view of DNS records mapping Host machine-id -> IP address
-- of its corresponding Host BMC interface.
--
-- For example: fm100h12987asd987awe98a.bmc.dev3.frg.nvidia.com
--
CREATE VIEW dns_records_bmc_host_id AS
SELECT
    concat(machine_interfaces.machine_id, '.bmc.', domains.name, '.') AS q_name,
    cast((machine_topologies.topology -> 'bmc_info' ->> 'ip') as inet) AS resource_record
FROM
    machine_interfaces
    JOIN machine_topologies ON ((machine_interfaces.machine_id = machine_topologies.machine_id)
            AND (machine_interfaces.machine_id != machine_interfaces.attached_dpu_machine_id))
    JOIN domains ON (domains.id = machine_interfaces.domain_id)
WHERE
    machine_interfaces.machine_id IS NOT NULL;

--
-- Name: dns_records_bmc_dpu_id; Type: TABLE; Schema: public; Owner: carbide_development
--
-- This is a view of DNS records mapping DPU machine-id -> IP address
-- of its DPU BMC interface. It's worth noting that we know it's a DPU
-- because the machine_id is the same as the attached_dpu_machine_id,
-- vs a host where the machine_id of the host is different than its
-- attached_dpu_machine_id.
--
-- For example: fm100d12987asd987awe98a.bmc.dev3.frg.nvidia.com
--
CREATE VIEW dns_records_bmc_dpu_id AS
SELECT
    concat(machine_interfaces.machine_id, '.bmc.', domains.name, '.') AS q_name,
    cast((machine_topologies.topology -> 'bmc_info' ->> 'ip') as inet) AS resource_record
FROM
    machine_interfaces
    JOIN machine_topologies ON ((machine_interfaces.machine_id = machine_topologies.machine_id)
            AND (machine_interfaces.machine_id = machine_interfaces.attached_dpu_machine_id))
    JOIN domains ON (domains.id = machine_interfaces.domain_id)
WHERE
    machine_interfaces.machine_id IS NOT NULL;

--
-- Name: dns_records; Type: TABLE; Schema: public; Owner: carbide_development
--
-- This is a view of all DNS record reviews for both the "pretty names" and
-- machine-ID derived names of Host & DPU admin and BMC interfaces, allowing
-- for discovery/access based on the name, without needing to CLI/API calls
-- to find the IP address for a given ID/name.
CREATE VIEW dns_records AS
SELECT
    *
FROM
    dns_records_shortname_combined
    FULL JOIN dns_records_adm_combined USING (q_name, resource_record)
    FULL JOIN dns_records_bmc_host_id USING (q_name, resource_record)
    FULL JOIN dns_records_bmc_dpu_id USING (q_name, resource_record);
