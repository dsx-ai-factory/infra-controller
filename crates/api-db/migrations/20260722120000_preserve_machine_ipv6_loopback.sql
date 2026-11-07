-- A pre-#2389 API pod replaces the complete network_config document without
-- fields it does not know. Preserve the IPv6 loopback across mixed-version
-- upgrades and rollbacks, while still allowing current writers to clear it
-- with an explicit JSON null. This guard can be retired once every supported
-- API version preserves loopback_ip_v6.
CREATE FUNCTION preserve_machine_ipv6_loopback()
RETURNS TRIGGER AS $$
BEGIN
    IF OLD.network_config ? 'loopback_ip_v6'
       AND NOT NEW.network_config ? 'loopback_ip_v6' THEN
        NEW.network_config := jsonb_set(
            NEW.network_config,
            '{loopback_ip_v6}',
            OLD.network_config -> 'loopback_ip_v6',
            true
        );
    END IF;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER preserve_machine_ipv6_loopback
BEFORE UPDATE OF network_config ON machines
FOR EACH ROW
EXECUTE FUNCTION preserve_machine_ipv6_loopback();
