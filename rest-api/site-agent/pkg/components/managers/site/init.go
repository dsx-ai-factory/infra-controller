// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package site

// Init initializes the Site manager.
func (api *API) Init() {
	ManagerAccess.Data.EB.Log.Info().Msg("Site: Initializing")
}

// GetState returns Site manager state.
func (api *API) GetState() []string {
	return []string{}
}
