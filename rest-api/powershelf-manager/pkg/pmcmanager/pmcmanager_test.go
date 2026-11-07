// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package pmcmanager

import (
	"context"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/redfish"

	"github.com/stretchr/testify/assert"
)

func TestRedfishTx_NilPMC(t *testing.T) {
	pm := &PmcManager{}

	err := pm.RedfishTx(context.Background(), nil, func(_ *redfish.RedfishClient) error {
		t.Fatal("tx should not be called with nil PMC")
		return nil
	})

	assert.Error(t, err)
	assert.Contains(t, err.Error(), "null PMC")
}
