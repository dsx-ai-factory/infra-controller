-- Allow putting a machine into maintenance mode

ALTER TABLE machines
  ADD column maintenance_reference VARCHAR(256) NULL,
  ADD column maintenance_start_time timestamp with time zone NULL;
