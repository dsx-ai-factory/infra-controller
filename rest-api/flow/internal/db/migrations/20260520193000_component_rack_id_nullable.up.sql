-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Allow ingesting a component before it has been assigned to a rack.
-- The application layer still validates the referenced rack when rack_id is
-- non-NULL; there is no FK on component.rack_id today, so dropping NOT NULL
-- is sufficient on the DB side.
ALTER TABLE component ALTER COLUMN rack_id DROP NOT NULL;
