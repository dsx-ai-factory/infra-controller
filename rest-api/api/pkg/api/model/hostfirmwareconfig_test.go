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

const (
	testArtifactSHA = "a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef123456"
	testScriptSHA   = "c60b4c1c2d46f7b9e50d7d2e2d77dbfbe7884030b0e9a86a4636b4bc85d8609e"
	testDriveModel  = "ST24000NM005H"
)

func testUpgradeScript() *APIHostFirmwareUpgradeScript {
	return &APIHostFirmwareUpgradeScript{
		URL:                            "https://scripts.example.invalid/upgrade.sh",
		Sha256:                         testScriptSHA,
		ExecutionTimeoutSeconds:        3600,
		ArtifactDownloadTimeoutSeconds: 600,
	}
}

func testVersionConfig(script *APIHostFirmwareUpgradeScript) APIHostFirmwareVersionConfig {
	return APIHostFirmwareVersionConfig{
		Version:       "ET07",
		Default:       true,
		Artifacts:     []APIHostFirmwareArtifact{{URL: "https://firmware.example.invalid/et07.lod", Sha256: cutil.GetPtr(testArtifactSHA)}},
		UpgradeScript: script,
	}
}

func testComponentConfig(componentType HostFirmwareComponentType, driveModel *string) APIHostFirmwareComponentConfig {
	return APIHostFirmwareComponentConfig{
		Type:       componentType,
		DriveModel: driveModel,
		Firmware:   []APIHostFirmwareVersionConfig{testVersionConfig(testUpgradeScript())},
	}
}

func testDriveRef(model string) APIHostFirmwareComponentRef {
	return APIHostFirmwareComponentRef{Type: HostFirmwareComponentTypeDrive, DriveModel: cutil.GetPtr(model)}
}

// assertValidationError checks a Validate result: no error when errPath is
// empty, otherwise an error mentioning errPath.
func assertValidationError(t *testing.T, err error, errPath string) {
	t.Helper()
	if errPath == "" {
		assert.NoError(t, err)
		return
	}
	require.Error(t, err)
	assert.Contains(t, err.Error(), errPath)
}

func TestAPIHostFirmwareComponentRef_Validate(t *testing.T) {
	tests := []struct {
		name    string
		ref     APIHostFirmwareComponentRef
		errPath string
	}{
		{name: "component type without a drive model", ref: APIHostFirmwareComponentRef{Type: HostFirmwareComponentTypeBMC}},
		{name: "drive with its model", ref: testDriveRef(testDriveModel)},
		{name: "drive model at the length limit", ref: testDriveRef(strings.Repeat("M", maxHostFirmwareDriveModelLength))},
		{name: "unknown component type", ref: APIHostFirmwareComponentRef{Type: "Disk"}, errPath: "type"},
		{name: "drive without its model", ref: APIHostFirmwareComponentRef{Type: HostFirmwareComponentTypeDrive}, errPath: "driveModel"},
		{name: "drive with an empty model", ref: testDriveRef(""), errPath: "driveModel"},
		{name: "drive model with surrounding whitespace", ref: testDriveRef(" " + testDriveModel), errPath: "driveModel"},
		{name: "drive model over the length limit", ref: testDriveRef(strings.Repeat("M", maxHostFirmwareDriveModelLength+1)), errPath: "driveModel"},
		{
			name:    "drive model on another component type",
			ref:     APIHostFirmwareComponentRef{Type: HostFirmwareComponentTypeBMC, DriveModel: cutil.GetPtr(testDriveModel)},
			errPath: "driveModel",
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assertValidationError(t, tc.ref.Validate(), tc.errPath)
		})
	}
}

func TestAPIHostFirmwareComponentConfig_Validate(t *testing.T) {
	withPreingest := func(component APIHostFirmwareComponentConfig, preingest string) APIHostFirmwareComponentConfig {
		component.PreingestUpgradeWhenBelow = cutil.GetPtr(preingest)
		return component
	}
	cx7 := testComponentConfig(HostFirmwareComponentTypeCx7, nil)
	drive := testComponentConfig(HostFirmwareComponentTypeDrive, cutil.GetPtr(testDriveModel))
	// No upgradeScript, so only the Drive rule can require the sha256.
	unhashedVersion := testVersionConfig(nil)
	unhashedVersion.Artifacts[0].Sha256 = nil
	unhashedDrive := testComponentConfig(HostFirmwareComponentTypeDrive, cutil.GetPtr(testDriveModel))
	unhashedDrive.Firmware = []APIHostFirmwareVersionConfig{unhashedVersion}

	tests := []struct {
		name      string
		component APIHostFirmwareComponentConfig
		errPath   string
	}{
		{name: "without preingestUpgradeWhenBelow", component: cx7},
		{name: "preingestUpgradeWhenBelow with surrounding whitespace", component: withPreingest(cx7, " 28.48.1000 ")},
		{name: "drive component", component: drive},
		{name: "blank preingestUpgradeWhenBelow", component: withPreingest(cx7, "   "), errPath: "preingestUpgradeWhenBelow"},
		{name: "preingestUpgradeWhenBelow on a drive", component: withPreingest(drive, "ET07"), errPath: "preingestUpgradeWhenBelow"},
		{name: "invalid drive model", component: testComponentConfig(HostFirmwareComponentTypeDrive, nil), errPath: "driveModel"},
		{name: "drive artifact without sha256", component: unhashedDrive, errPath: "sha256"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assertValidationError(t, tc.component.Validate(), tc.errPath)
		})
	}
}

func TestAPIHostFirmwareComponentConfig_ToProto(t *testing.T) {
	tests := []struct {
		name          string
		component     APIHostFirmwareComponentConfig
		wantType      corev1.HostFirmwareComponentType
		wantDrive     *string
		wantPreingest *string
	}{
		{
			name: "trims preingestUpgradeWhenBelow",
			component: APIHostFirmwareComponentConfig{
				Type:                      HostFirmwareComponentTypeCx7,
				PreingestUpgradeWhenBelow: cutil.GetPtr(" 28.48.1000 "),
			},
			wantType:      corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_CX7,
			wantPreingest: cutil.GetPtr("28.48.1000"),
		},
		{
			name:      "keeps the drive model",
			component: testComponentConfig(HostFirmwareComponentTypeDrive, cutil.GetPtr(testDriveModel)),
			wantType:  corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_DRIVE,
			wantDrive: cutil.GetPtr(testDriveModel),
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			proto := tc.component.ToProto()
			assert.Equal(t, tc.wantType, proto.Type)
			assert.Equal(t, tc.wantDrive, proto.DriveModel)
			assert.Equal(t, tc.wantPreingest, proto.PreingestUpgradeWhenBelow)
		})
	}
}

func TestAPIHostFirmwareVersionConfig_Validate(t *testing.T) {
	withArtifactSHA := func(sha256 *string) APIHostFirmwareVersionConfig {
		version := testVersionConfig(nil)
		version.Artifacts[0].Sha256 = sha256
		return version
	}
	withScriptArtifactSHA := func(sha256 *string) APIHostFirmwareVersionConfig {
		version := testVersionConfig(testUpgradeScript())
		version.Artifacts[0].Sha256 = sha256
		return version
	}
	withScript := func(change func(*APIHostFirmwareUpgradeScript)) APIHostFirmwareVersionConfig {
		script := testUpgradeScript()
		change(script)
		return testVersionConfig(script)
	}

	tests := []struct {
		name    string
		version APIHostFirmwareVersionConfig
		errPath string
	}{
		{name: "artifact without sha256", version: withArtifactSHA(nil)},
		{name: "artifact with lowercase sha256", version: withArtifactSHA(cutil.GetPtr(testArtifactSHA))},
		{name: "artifact sha256 that is not hex", version: withArtifactSHA(cutil.GetPtr("not-a-sha")), errPath: "sha256"},
		{name: "artifact sha256 in uppercase", version: withArtifactSHA(cutil.GetPtr(strings.ToUpper(testArtifactSHA))), errPath: "sha256"},
		{name: "artifact sha256 too short", version: withArtifactSHA(cutil.GetPtr(testArtifactSHA[:63])), errPath: "sha256"},
		{name: "upgrade script", version: testVersionConfig(testUpgradeScript())},
		{name: "upgrade script with an artifact without sha256", version: withScriptArtifactSHA(nil), errPath: "sha256"},
		{name: "upgrade script without a URL", version: withScript(func(s *APIHostFirmwareUpgradeScript) { s.URL = "" }), errPath: "upgradeScript"},
		{
			name:    "upgrade script sha256 in uppercase",
			version: withScript(func(s *APIHostFirmwareUpgradeScript) { s.Sha256 = strings.ToUpper(testScriptSHA) }),
			errPath: "sha256",
		},
		{
			name:    "upgrade script with zero execution timeout",
			version: withScript(func(s *APIHostFirmwareUpgradeScript) { s.ExecutionTimeoutSeconds = 0 }),
			errPath: "executionTimeoutSeconds",
		},
		{
			name:    "upgrade script with zero download timeout",
			version: withScript(func(s *APIHostFirmwareUpgradeScript) { s.ArtifactDownloadTimeoutSeconds = 0 }),
			errPath: "artifactDownloadTimeoutSeconds",
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assertValidationError(t, tc.version.Validate(), tc.errPath)
		})
	}
}

func TestAPIHostFirmwareVersionConfig_ToProto(t *testing.T) {
	exclusive := true
	tests := []struct {
		name          string
		version       APIHostFirmwareVersionConfig
		wantExclusive *bool
		wantScript    *corev1.HostFirmwareUpgradeScript
	}{
		{
			name: "preingestionExclusiveConfig without a script",
			version: APIHostFirmwareVersionConfig{
				Version:                     "28.47.2682",
				Default:                     true,
				PreingestionExclusiveConfig: &exclusive,
			},
			wantExclusive: &exclusive,
		},
		{
			name:    "upgrade script",
			version: testVersionConfig(testUpgradeScript()),
			wantScript: &corev1.HostFirmwareUpgradeScript{
				Url:                            "https://scripts.example.invalid/upgrade.sh",
				Sha256:                         testScriptSHA,
				ExecutionTimeoutSeconds:        3600,
				ArtifactDownloadTimeoutSeconds: 600,
			},
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			proto := tc.version.ToProto()
			assert.Equal(t, tc.wantExclusive, proto.PreingestionExclusiveConfig)
			assert.Equal(t, tc.wantScript, proto.UpgradeScript)
		})
	}
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

func TestAPIHostFirmwareConfigCreateOrUpdateRequest_Validate(t *testing.T) {
	request := func(components []APIHostFirmwareComponentConfig, ordering ...APIHostFirmwareComponentRef) APIHostFirmwareConfigCreateOrUpdateRequest {
		return APIHostFirmwareConfigCreateOrUpdateRequest{
			SiteID:     "00000000-0000-0000-0000-000000000001",
			Vendor:     "Lenovo",
			Model:      "ThinkSystem HS350X V3",
			Components: components,
			Ordering:   ordering,
		}
	}
	bmcRef := APIHostFirmwareComponentRef{Type: HostFirmwareComponentTypeBMC}
	hdd := testComponentConfig(HostFirmwareComponentTypeDrive, cutil.GetPtr(testDriveModel))
	ssd := testComponentConfig(HostFirmwareComponentTypeDrive, cutil.GetPtr("SSDPF2KE016T1O"))
	bmc := testComponentConfig(HostFirmwareComponentTypeBMC, nil)

	tests := []struct {
		name    string
		request APIHostFirmwareConfigCreateOrUpdateRequest
		errPath string
	}{
		{
			name:    "components in order",
			request: request([]APIHostFirmwareComponentConfig{bmc}, bmcRef),
		},
		{
			name:    "drives of different models in order",
			request: request([]APIHostFirmwareComponentConfig{hdd, ssd}, testDriveRef("SSDPF2KE016T1O"), testDriveRef(testDriveModel)),
		},
		{
			name:    "invalid component",
			request: request([]APIHostFirmwareComponentConfig{testComponentConfig(HostFirmwareComponentTypeDrive, nil)}),
			errPath: "driveModel",
		},
		{
			name:    "same drive model twice",
			request: request([]APIHostFirmwareComponentConfig{hdd, hdd}),
			errPath: "components[1].type",
		},
		{
			name:    "invalid ordering entry",
			request: request([]APIHostFirmwareComponentConfig{hdd}, APIHostFirmwareComponentRef{Type: HostFirmwareComponentTypeDrive}),
			errPath: "ordering[0]",
		},
		{
			name:    "same type twice in ordering",
			request: request([]APIHostFirmwareComponentConfig{bmc}, bmcRef, bmcRef),
			errPath: "ordering[1]",
		},
		{
			name:    "same drive model twice in ordering",
			request: request([]APIHostFirmwareComponentConfig{hdd}, testDriveRef(testDriveModel), testDriveRef(testDriveModel)),
			errPath: "ordering[1]",
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assertValidationError(t, tc.request.Validate(), tc.errPath)
		})
	}
}

func TestAPIHostFirmwareConfigCreateOrUpdateRequest_ToProto(t *testing.T) {
	proto := APIHostFirmwareConfigCreateOrUpdateRequest{
		Vendor: "Lenovo",
		Model:  "ThinkSystem HS350X V3",
		Components: []APIHostFirmwareComponentConfig{
			testComponentConfig(HostFirmwareComponentTypeBMC, nil),
			testComponentConfig(HostFirmwareComponentTypeDrive, cutil.GetPtr(testDriveModel)),
		},
		Ordering: []APIHostFirmwareComponentRef{{Type: HostFirmwareComponentTypeBMC}, testDriveRef(testDriveModel)},
	}.ToProto()

	assert.Equal(t, "Lenovo", proto.Vendor)
	require.Len(t, proto.Components, 2)
	assert.Equal(t, cutil.GetPtr(testDriveModel), proto.Components[1].DriveModel)
	assert.Equal(t, []*corev1.HostFirmwareComponentRef{
		{Type: corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_BMC},
		{Type: corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_DRIVE, DriveModel: cutil.GetPtr(testDriveModel)},
	}, proto.Ordering)
}

func TestAPIHostFirmwareConfig_FromProto(t *testing.T) {
	cx7 := corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_CX7
	drive := corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_DRIVE
	bmc := corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_BMC
	preingest := "28.48.1000"
	exclusive := true
	powerDrainsNeeded := uint32(math.MaxUint32)

	tests := []struct {
		name  string
		proto *corev1.HostFirmwareConfigResponse
		check func(t *testing.T, resp *APIHostFirmwareConfig)
	}{
		{
			name: "component and version fields",
			proto: &corev1.HostFirmwareConfigResponse{
				Vendor:              "Nvidia",
				Model:               "DGXH100",
				ExplicitStartNeeded: true,
				Ordering:            []*corev1.HostFirmwareComponentRef{{Type: cx7}},
				Components: []*corev1.HostFirmwareComponentConfigResponse{{
					Type:                      cx7,
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
			},
			check: func(t *testing.T, resp *APIHostFirmwareConfig) {
				require.Len(t, resp.Components, 1)
				component := resp.Components[0]
				assert.Nil(t, component.DriveModel)
				assert.Equal(t, cutil.GetPtr("^CX7_[0-9]+$"), component.CurrentVersionDetectionRegEx)
				assert.Equal(t, &preingest, component.PreingestUpgradeWhenBelow)
				require.Len(t, component.Firmware, 1)
				require.NotNil(t, component.Firmware[0].PreingestionExclusiveConfig)
				assert.True(t, *component.Firmware[0].PreingestionExclusiveConfig)
				assert.Equal(t, &powerDrainsNeeded, component.Firmware[0].PowerDrainsNeeded)
				assert.Nil(t, component.Firmware[0].UpgradeScript)
				assert.Equal(t, []APIHostFirmwareComponentRef{{Type: HostFirmwareComponentTypeCx7}}, resp.Ordering)
			},
		},
		{
			name: "drive component, upgrade script and ordering",
			proto: &corev1.HostFirmwareConfigResponse{
				Vendor: "Lenovo",
				Model:  "ThinkSystem HS350X V3",
				Ordering: []*corev1.HostFirmwareComponentRef{
					{Type: bmc},
					{Type: drive, DriveModel: cutil.GetPtr(testDriveModel)},
				},
				Components: []*corev1.HostFirmwareComponentConfigResponse{{
					Type:       drive,
					DriveModel: cutil.GetPtr(testDriveModel),
					Firmware: []*corev1.HostFirmwareVersionConfig{{
						Version: "ET07",
						UpgradeScript: &corev1.HostFirmwareUpgradeScript{
							Url:                            "https://scripts.example.invalid/upgrade.sh",
							Sha256:                         testScriptSHA,
							ExecutionTimeoutSeconds:        3600,
							ArtifactDownloadTimeoutSeconds: 600,
						},
					}},
				}},
			},
			check: func(t *testing.T, resp *APIHostFirmwareConfig) {
				assert.Equal(t, []APIHostFirmwareComponentRef{
					{Type: HostFirmwareComponentTypeBMC},
					testDriveRef(testDriveModel),
				}, resp.Ordering)
				require.Len(t, resp.Components, 1)
				assert.Equal(t, HostFirmwareComponentTypeDrive, resp.Components[0].Type)
				assert.Equal(t, cutil.GetPtr(testDriveModel), resp.Components[0].DriveModel)
				assert.Equal(t, testUpgradeScript(), resp.Components[0].Firmware[0].UpgradeScript)
			},
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			resp := &APIHostFirmwareConfig{}
			resp.FromProto(tc.proto)
			tc.check(t, resp)
		})
	}
}
