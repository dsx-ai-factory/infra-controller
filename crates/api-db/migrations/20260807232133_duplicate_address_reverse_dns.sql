-- Forward-domain duplicates remain supported. Normalized reverse-zone names
-- identify the same DNS zone, so live dotted and non-dotted spellings must not
-- coexist. Existing normalized duplicates make this migration fail without
-- changing any rows so an operator can reconcile them deliberately.
CREATE UNIQUE INDEX domains_live_reverse_zone_name_key
    ON domains ((lower(rtrim(name, '.'))))
    WHERE deleted IS NULL
      AND (
          lower(rtrim(name, '.')) LIKE '%.in-addr.arpa'
          OR lower(rtrim(name, '.')) LIKE '%.ip6.arpa'
      );

-- PTR resolution probes overlay addresses directly when deciding whether an
-- address has one owner or is ambiguous across VPCs.
CREATE INDEX instance_addresses_address_idx
    ON instance_addresses (address);
