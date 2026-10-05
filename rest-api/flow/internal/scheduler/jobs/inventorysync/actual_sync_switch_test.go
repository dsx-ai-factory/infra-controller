// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"
	"os"
	"testing"

	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"

	cdb "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/db/model"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// TestSyncSwitchNvosIPs covers the description merge: a resolved NVOS IP is
// written under nvosIPDescriptionKey while other keys are preserved, and a
// switch Core has not resolved an NVOS IP for is left untouched.
func TestSyncSwitchNvosIPs(t *testing.T) {
	ctx := context.Background()

	if os.Getenv("DB_PORT") == "" {
		log.Warn().Msgf("Not running unit test due to no DB environment specified")
		t.SkipNow()
	}

	dbConf, err := cdb.ConfigFromEnv()
	assert.Nil(t, err)
	pool, err := utils.UnitTestDB(ctx, t, dbConf)
	assert.Nil(t, err)

	grpcMock := nicoapi.NewMockClient()

	rack := model.Rack{
		Name:         "test-rack-nvos",
		Manufacturer: "TestMfg",
		SerialNumber: "rack-serial-nvos",
	}
	assert.Nil(t, rack.Create(ctx, pool.DB))

	nvSwitchType := devicetypes.ComponentTypeToString(devicetypes.ComponentTypeNVSwitch)

	// Switch Core has resolved an NVOS IP for; carries a pre-existing
	// description key that must survive the merge.
	resolved := model.Component{
		SerialNumber: "nvos-serial-1",
		Manufacturer: "TestMfg",
		RackID:       rack.ID,
		Type:         nvSwitchType,
		Description:  map[string]any{"existing": "keep-me"},
	}
	assert.Nil(t, resolved.Create(ctx, pool.DB))

	// Switch without a resolved NVOS endpoint; description must stay nil.
	unresolved := model.Component{
		SerialNumber: "nvos-serial-2",
		Manufacturer: "TestMfg",
		RackID:       rack.ID,
		Type:         nvSwitchType,
	}
	assert.Nil(t, unresolved.Create(ctx, pool.DB))

	const resolvedSwitchID = "core-switch-id-1"
	const unresolvedSwitchID = "core-switch-id-2"
	grpcMock.SetSwitchNvosIP(resolvedSwitchID, "10.130.4.21")

	componentsBySwitchID := map[string]*model.Component{
		resolvedSwitchID:   &resolved,
		unresolvedSwitchID: &unresolved,
	}

	syncSwitchNvosIPs(ctx, pool, grpcMock, componentsBySwitchID)

	var updated model.Component
	assert.Nil(t, pool.DB.NewSelect().Model(&updated).Where("id = ?", resolved.ID).Scan(ctx))
	assert.Equal(t, "10.130.4.21", updated.Description[nvosIPDescriptionKey])
	assert.Equal(t, "keep-me", updated.Description["existing"])

	var untouched model.Component
	assert.Nil(t, pool.DB.NewSelect().Model(&untouched).Where("id = ?", unresolved.ID).Scan(ctx))
	_, hasKey := untouched.Description[nvosIPDescriptionKey]
	assert.False(t, hasKey)
}
