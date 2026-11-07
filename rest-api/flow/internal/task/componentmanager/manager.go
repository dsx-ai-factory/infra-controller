// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package componentmanager defines the manager contracts used to dispatch task
// operations. Each manager owns its descriptor metadata, which is used by the
// registry to validate configured implementations and supported capabilities.
package componentmanager

import (
	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
)

// ComponentManager defines the common identity and metadata every component
// manager must expose. Operation methods live on capability-specific
// interfaces so managers only implement the operations they support.
type ComponentManager interface {
	// Descriptor returns the component manager metadata for this manager.
	Descriptor() cmcatalog.Descriptor
}
