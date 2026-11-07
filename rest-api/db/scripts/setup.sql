-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Connect as postgres user to template1 DB to run this script
-- Example: PGPASSWORD=postgres psql -U postgres -p 30432 -d template1 < scripts/setup.sql
-- Create extension for pg_trgm
CREATE EXTENSION IF NOT EXISTS pg_trgm;
-- Create Nico DB and user
CREATE DATABASE nico WITH ENCODING 'UTF8';
-- Password should be changed before use in environments deployed in Cloud
CREATE USER nico WITH PASSWORD 'nico';
-- Grant all privileges on Nico DB to Nico user
GRANT ALL PRIVILEGES ON DATABASE nico TO nico;
