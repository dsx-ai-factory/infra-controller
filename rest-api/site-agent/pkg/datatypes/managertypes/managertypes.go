// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managertypes

import (
	bootstraptypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/bootstrap"
	coregrpctypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/coregrpc"
	flowgrpctypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/flowgrpc"
	workflowtypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/workflow"
)

// Managers - manager ds
type Managers struct {
	Version string
	// All the datastructures of Managers below
	Workflow  *workflowtypes.Workflow
	CoreGrpc  *coregrpctypes.CoreGrpc
	FlowGrpc  *flowgrpctypes.FlowGrpc
	Bootstrap *bootstraptypes.Bootstrap
}

// NewManagerType - get new type of all managers
func NewManagerType() *Managers {
	return &Managers{
		Version: "0.0.1",
		// All the managers below
		Workflow:  workflowtypes.NewWorkflowInstance(),
		CoreGrpc:  coregrpctypes.NewCoreGrpcInstance(),
		FlowGrpc:  flowgrpctypes.NewFlowGrpcInstance(),
		Bootstrap: bootstraptypes.NewBootstrapInstance(),
	}
}
