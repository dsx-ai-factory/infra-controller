// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package elektra

import (
	"testing"
	"time"
)

func TestCoreGrpcClientReinitializationOnCertRenewal(t *testing.T) {
	// Initial setup with TestInitElektra which configures the Core gRPC client with initial certificates
	TestInitElektra(t)
	initialVersion := testElektra.manager.API.CoreGrpc.GetGrpcClientVersion()

	// Regenerate and replace the certificates to simulate renewal
	SetupTestCerts(t, testElektraTypes.Conf.CoreGrpc.ClientCertPath, testElektraTypes.Conf.CoreGrpc.ClientKeyPath, testElektraTypes.Conf.CoreGrpc.ServerCAPath)

	// Wait a few seconds to allow any background processes to complete
	time.Sleep(time.Second * 5)
	renewedVersion := testElektra.manager.API.CoreGrpc.GetGrpcClientVersion()

	if renewedVersion > initialVersion {
		t.Logf("The Core gRPC client was successfully reinitialized from version %d to %d.", initialVersion, renewedVersion)
	} else {
		t.Errorf("The Core gRPC client was not reinitialized as expected. It remains at version %d.", initialVersion)
	}
}
