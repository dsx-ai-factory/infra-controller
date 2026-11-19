-- Records the declared logical interface name of an Astra card's registration
-- lane. Null covers every interface whose name the registering caller does not
-- know, including legacy indexed Astra rows and all non-Astra interfaces.
ALTER TABLE dpa_interfaces ADD COLUMN interface_name text;
