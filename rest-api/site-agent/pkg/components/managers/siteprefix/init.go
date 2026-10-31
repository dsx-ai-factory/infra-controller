// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

// Init initializes the SitePrefix inventory manager.
func (api *API) Init() {
	ManagerAccess.Data.EB.Log.Info().Msg("SitePrefix: Initializing inventory manager")
}
