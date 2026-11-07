// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package resolver

import (
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

// ComponentTypeToFlow converts an inventory component type into its canonical
// public Flow representation.
func ComponentTypeToFlow(
	componentType devicetypes.ComponentType,
) (flowtypes.ComponentType, error) {
	switch componentType {
	case devicetypes.ComponentTypeCompute:
		return flowtypes.ComponentTypeCompute, nil
	case devicetypes.ComponentTypeNVSwitch:
		return flowtypes.ComponentTypeNVSwitch, nil
	case devicetypes.ComponentTypePowerShelf:
		return flowtypes.ComponentTypePowerShelf, nil
	case devicetypes.ComponentTypeToRSwitch:
		return flowtypes.ComponentTypeTORSwitch, nil
	case devicetypes.ComponentTypeUMS:
		return flowtypes.ComponentTypeUMS, nil
	case devicetypes.ComponentTypeCDU:
		return flowtypes.ComponentTypeCDU, nil
	default:
		return flowtypes.ComponentTypeUnknown, fmt.Errorf(
			"unknown inventory component type %d",
			componentType,
		)
	}
}
