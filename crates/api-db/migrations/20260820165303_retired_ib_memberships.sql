-- Retired memberships stay recorded after their `Machine` and `Instance` records
-- are gone and after UFM removes them. A live `Instance` can reuse the exact
-- membership and remove its record.
CREATE TABLE retired_ib_memberships (
    fabric TEXT NOT NULL,
    pkey INTEGER NOT NULL CHECK (pkey BETWEEN 0 AND 32767),
    guid TEXT NOT NULL,
    PRIMARY KEY (fabric, pkey, guid)
);
