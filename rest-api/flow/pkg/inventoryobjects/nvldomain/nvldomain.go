// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nvldomain

import (
	"errors"

	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/google/uuid"
)

type NVLDomain struct {
	Identifier      identifier.Identifier   `json:"identifier"`
	NMXCClusterID   *uuid.UUID              `json:"nmxc_cluster_id"`
	RackIdentifiers []identifier.Identifier `json:"rack_identifiers"`
}

// ListOptions selects the public domain surface and its stable name ordering.
type ListOptions struct {
	ExternalOnly bool
	Descending   bool
}

func (d *NVLDomain) Validate() error {
	if d == nil {
		return errors.New("nvl domain is not specfied")
	}

	return d.Identifier.Validate()
}

func (d *NVLDomain) ID() uuid.UUID {
	return d.Identifier.ID
}

func (d *NVLDomain) Name() string {
	return d.Identifier.Name
}
