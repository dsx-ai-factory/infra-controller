-- Add an predicted_machine_interfaces table so that we can take note of which interfaces exist on a zero-DPU machine,
-- then migrate them to real machine_interfaces once we observe a DHCP request.
CREATE TABLE predicted_machine_interfaces
(
    id          uuid DEFAULT gen_random_uuid() NOT NULL,
    machine_id  character varying(64) NOT NULL,
    mac_address macaddr                        NOT NULL,
    expected_network_segment_type network_segment_type_t NOT NULL,
    PRIMARY KEY (id),
    CONSTRAINT predicted_machine_interfaces_machine_id_fkey FOREIGN KEY(machine_id) REFERENCES machines(id) ON UPDATE CASCADE ON DELETE CASCADE,
    CONSTRAINT predicted_machine_interfaces_unique_mac_address UNIQUE (mac_address)
);