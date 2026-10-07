ALTER TABLE expected_machines
    ADD COLUMN id uuid UNIQUE;

UPDATE expected_machines
    SET id = gen_random_uuid()
    WHERE id IS NULL;
