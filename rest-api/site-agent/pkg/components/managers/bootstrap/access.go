// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package bootstrap

import (
	Manager "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/managerapi"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
)

// ManagerAccess - access to all managers
var ManagerAccess *Manager.ManagerAccess

// BoostrapAPI - all API interface
// nolint
type BoostrapAPI struct{}

// NewBootstrapManager - returns a new instance of helm manager
func NewBootstrapManager(superForge *elektratypes.Elektra, superAPI *Manager.ManagerAPI, superConf *Manager.ManagerConf) *BoostrapAPI {
	ManagerAccess = &Manager.ManagerAccess{
		Data: &Manager.ManagerData{
			EB: superForge,
		},
		API:  superAPI,
		Conf: superConf,
	}
	return &BoostrapAPI{}
}
