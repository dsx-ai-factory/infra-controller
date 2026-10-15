-- Retain Scout MLX reports independently of DPA interface configuration.
ALTER TABLE machines ADD COLUMN mlx_device_observation JSONB;
