// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sku

// RegisterSubscriber registers SKU CRUD workflows and activities with Temporal
// (inventory-only: no create/update/delete workflows are registered).
func (api *API) RegisterSubscriber() error {
	ManagerAccess.Data.EB.Log.Info().Msg("SKU: Registering CRUD workflows and activities")

	ManagerAccess.Data.EB.Log.Info().Msg("SKU: No CRUD workflows for SKU (inventory-only resource)")

	return nil
}
