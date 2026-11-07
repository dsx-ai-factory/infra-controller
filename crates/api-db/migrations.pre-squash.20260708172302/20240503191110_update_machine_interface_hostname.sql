-- Add migration script here

UPDATE machine_interfaces
SET hostname=subquery.hostname
FROM (select replace(text(host(address)), '.', '-') as hostname, machine_interfaces.id as interface_id
                FROM machine_interfaces  JOIN machine_interface_addresses ON machine_interfaces.id = machine_interface_addresses.interface_id
                WHERE family(address) = 4) AS subquery
WHERE machine_interfaces.id = subquery.interface_id AND machine_interfaces.hostname != subquery.hostname;