-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

ALTER TABLE nvldomain DROP COLUMN nmxc_cluster_id;
ALTER TABLE nvldomain DROP COLUMN external_id;
ALTER TABLE rack DROP COLUMN rack_group_id;
