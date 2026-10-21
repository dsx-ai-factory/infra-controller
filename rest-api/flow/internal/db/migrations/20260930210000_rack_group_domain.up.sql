-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

ALTER TABLE rack ADD COLUMN rack_group_id text;
ALTER TABLE nvldomain ADD COLUMN external_id text UNIQUE;
ALTER TABLE nvldomain ADD COLUMN nmxc_cluster_id uuid;
