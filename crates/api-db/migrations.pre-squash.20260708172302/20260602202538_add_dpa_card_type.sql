-- NIC card type for DPA interfaces (SVPC vs ASTRA).
CREATE TYPE dpa_interface_type AS ENUM ('Svpc', 'Astra');

ALTER TABLE dpa_interfaces
    ADD COLUMN interface_type dpa_interface_type;

UPDATE dpa_interfaces SET interface_type='Svpc';

ALTER TABLE dpa_interfaces
    ALTER COLUMN interface_type SET NOT NULL;
