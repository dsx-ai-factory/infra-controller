-- Add migration script here
ALTER TABLE
  machine_validation
ADD
  COLUMN description VARCHAR(256),
ADD
  COLUMN total INT,
ADD
  COLUMN completed INT,
ADD
  COLUMN state VARCHAR(64);

UPDATE
  machine_validation
SET
  state = 'InProgress',
  total = 1,
  completed = 1
where
  end_time IS NULL;

UPDATE
  machine_validation
SET
  state = 'Completed',
  total = 1,
  completed = 1
where
  end_time IS NOT NULL;