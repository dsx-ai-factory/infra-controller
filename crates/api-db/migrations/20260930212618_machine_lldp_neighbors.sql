-- LLDP neighbors reported at runtime by scout (host) and the DPU agent. Each UPDATED report
-- replaces the set; created_at records when that report was stored.
CREATE TABLE machine_lldp_neighbors (
    machine_id VARCHAR(64) NOT NULL
        REFERENCES machines(id) ON UPDATE CASCADE ON DELETE CASCADE,
    local_mac_address MACADDR NOT NULL,
    local_port TEXT NOT NULL,
    chassis_id_type TEXT NOT NULL,
    chassis_id_value TEXT NOT NULL,
    remote_port_type TEXT NOT NULL,
    remote_port_value TEXT NOT NULL,
    system_name TEXT NOT NULL,
    system_description TEXT NOT NULL,
    -- lldpd reports management addresses as text and they are not guaranteed to be IPs.
    management_addresses TEXT[] NOT NULL,
    med_serial TEXT,
    med_manufacturer TEXT,
    med_model TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (
        machine_id,
        local_mac_address,
        local_port,
        chassis_id_type,
        chassis_id_value,
        remote_port_type,
        remote_port_value
    )
);
