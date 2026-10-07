// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"math"
	"strings"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestAPIHostFirmwareComponentConfig_Validate_preingestUpgradeWhenBelow(t *testing.T) {
	validComponent := func(preingest *string) APIHostFirmwareComponentConfig {
		return APIHostFirmwareComponentConfig{
			Type:                      HostFirmwareComponentTypeCx7,
			PreingestUpgradeWhenBelow: preingest,
			Firmware: []APIHostFirmwareVersionConfig{{
				Version: "28.47.2682",
				Default: true,
				Artifacts: []APIHostFirmwareArtifact{{
					URL: "https://firmware.example.invalid/28.47.2682/fw.bin",
				}},
			}},
		}
	}

	assert.NoError(t, validComponent(nil).Validate())
	assert.NoError(t, validComponent(cutil.GetPtr("28.48.1000")).Validate())
	assert.NoError(t, validComponent(cutil.GetPtr(" 28.48.1000 ")).Validate())

	err := validComponent(cutil.GetPtr("   ")).Validate()
	require.Error(t, err)
	assert.Contains(t, err.Error(), "preingestUpgradeWhenBelow")
}

func TestAPIHostFirmwareComponentConfig_ToProto_preingestUpgradeWhenBelow(t *testing.T) {
	proto := APIHostFirmwareComponentConfig{
		Type:                      HostFirmwareComponentTypeCx7,
		PreingestUpgradeWhenBelow: cutil.GetPtr(" 28.48.1000 "),
		Firmware: []APIHostFirmwareVersionConfig{{
			Version: "28.47.2682",
			Default: true,
			Artifacts: []APIHostFirmwareArtifact{{
				URL: "https://firmware.example.invalid/28.47.2682/fw.bin",
			}},
		}},
	}.ToProto()

	require.NotNil(t, proto.PreingestUpgradeWhenBelow)
	assert.Equal(t, "28.48.1000", *proto.PreingestUpgradeWhenBelow)
}

func TestAPIHostFirmwareVersionConfig_Validate_artifactSha256(t *testing.T) {
	validSHA := "a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef123456"
	validVersion := func(sha256 *string) APIHostFirmwareVersionConfig {
		return APIHostFirmwareVersionConfig{
			Version: "28.47.2682",
			Default: true,
			Artifacts: []APIHostFirmwareArtifact{{
				URL:    "https://firmware.example.invalid/28.47.2682/fw.bin",
				Sha256: sha256,
			}},
		}
	}

	assert.NoError(t, validVersion(nil).Validate())
	assert.NoError(t, validVersion(cutil.GetPtr(validSHA)).Validate())

	err := validVersion(cutil.GetPtr("not-a-sha")).Validate()
	require.Error(t, err)
	assert.Contains(t, err.Error(), "sha256")

	err = validVersion(cutil.GetPtr(strings.ToUpper(validSHA))).Validate()
	require.Error(t, err)
	assert.Contains(t, err.Error(), "sha256")

	err = validVersion(cutil.GetPtr(validSHA[:63])).Validate()
	require.Error(t, err)
	assert.Contains(t, err.Error(), "sha256")
}

func TestAPIHostFirmwareVersionConfig_ToProto_preingestionExclusiveConfig(t *testing.T) {
	exclusive := true
	proto := APIHostFirmwareVersionConfig{
		Version:                     "28.47.2682",
		Default:                     true,
		PreingestionExclusiveConfig: &exclusive,
		Artifacts: []APIHostFirmwareArtifact{{
			URL: "https://firmware.example.invalid/28.47.2682/fw.bin",
		}},
	}.ToProto()

	require.NotNil(t, proto.PreingestionExclusiveConfig)
	assert.True(t, *proto.PreingestionExclusiveConfig)
}

func TestAPIHostFirmwareConfigDeleteRequest_ToProto(t *testing.T) {
	proto := APIHostFirmwareConfigDeleteRequest{
		SiteID: "00000000-0000-0000-0000-000000000001",
		Vendor: "Nvidia",
		Model:  "DGXH100",
	}.ToProto()

	assert.Equal(t, "Nvidia", proto.Vendor)
	assert.Equal(t, "DGXH100", proto.Model)
}

func TestAPIHostFirmwareConfigDeleteRequest_Validate(t *testing.T) {
	valid := APIHostFirmwareConfigDeleteRequest{
		SiteID: "00000000-0000-0000-0000-000000000001",
		Vendor: "Nvidia",
		Model:  "DGXH100",
	}
	assert.NoError(t, valid.Validate())

	err := APIHostFirmwareConfigDeleteRequest{
		SiteID: "00000000-0000-0000-0000-000000000001",
		Vendor: "Nvidia",
	}.Validate()
	require.Error(t, err)
}

func TestAPIHostFirmwareConfig_FromProto_newFields(t *testing.T) {
	preingest := "28.48.1000"
	exclusive := true
	powerDrainsNeeded := uint32(math.MaxUint32)
	proto := &corev1.HostFirmwareConfigResponse{
		Vendor:              "Nvidia",
		Model:               "DGXH100",
		ExplicitStartNeeded: true,
		Ordering:            []corev1.HostFirmwareComponentType{corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_CX7},
		Components: []*corev1.HostFirmwareComponentConfigResponse{{
			Type:                      corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_CX7,
			PreingestUpgradeWhenBelow: &preingest,
			CurrentVersionReportedAs:  cutil.GetPtr("^CX7_[0-9]+$"),
			Firmware: []*corev1.HostFirmwareVersionConfig{{
				Version:                     "28.47.2682",
				Default:                     true,
				PowerDrainsNeeded:           &powerDrainsNeeded,
				PreingestionExclusiveConfig: &exclusive,
				Artifacts: []*corev1.HostFirmwareArtifact{{
					Url: "https://firmware.example.invalid/28.47.2682/fw.bin",
				}},
			}},
		}},
	}

	resp := &APIHostFirmwareConfig{}
	resp.FromProto(proto)

	require.Len(t, resp.Components, 1)
	assert.Equal(t, cutil.GetPtr("^CX7_[0-9]+$"), resp.Components[0].CurrentVersionDetectionRegEx)
	assert.Equal(t, &preingest, resp.Components[0].PreingestUpgradeWhenBelow)
	require.Len(t, resp.Components[0].Firmware, 1)
	require.NotNil(t, resp.Components[0].Firmware[0].PreingestionExclusiveConfig)
	assert.True(t, *resp.Components[0].Firmware[0].PreingestionExclusiveConfig)
	assert.Equal(t, &powerDrainsNeeded, resp.Components[0].Firmware[0].PowerDrainsNeeded)
}
