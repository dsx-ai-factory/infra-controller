// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"encoding/base32"
	"encoding/json"
	"strings"
	"time"

	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	nicoprovider "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providers/nico"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	// detailsReadTimeout leaves ten seconds in the proxy's 40-second
	// activity budget for transport and response encoding.
	detailsReadTimeout = 30 * time.Second
	// detailsResponseReserve leaves assembly time before a shorter caller
	// deadline. The entry deadline applies to inventory and both data sources.
	detailsResponseReserve = time.Second
	// detailsResponseBytes bounds each component-details RPC's encoded result below
	// Temporal's 2 MiB payload ceiling. It uses the numeric allowance from
	// site-workflow/pkg/activity/inventory.go, applied here to the encoded proxy
	// wrapper rather than inventory's batching calculation.
	// The task count bound alone cannot limit recorded target arrays.
	detailsResponseBytes = 1945 * 1024
	// Core's typed hardware IDs have a seven-byte prefix and a Base32Hex hash.
	coreComponentIDLength       = 59
	coreComponentIDPrefixLength = 7
	coreComponentHashBytes      = 32
)

// componentPowerReader borrows the provider's existing connection; component detail
// requests neither create a Core client nor take ownership of its lifetime.
type componentPowerReader interface {
	GetComponentPowerDetails(context.Context, *corev1.GetComponentPowerDetailsRequest) (*corev1.GetComponentPowerDetailsResponse, error)
}

// componentFirmwareReader borrows the provider's passive firmware read capability.
type componentFirmwareReader interface {
	GetComponentFirmwareDetails(context.Context, *corev1.GetComponentFirmwareDetailsRequest) (*corev1.GetComponentFirmwareDetailsResponse, error)
}

// componentDetailsReader binds both passive capabilities to the same provider client.
type componentDetailsReader interface {
	componentPowerReader
	componentFirmwareReader
}

// detailsReaderFromRegistry lends the configured client's passive capabilities.
// An absent provider leaves Flow task reads available without inventing Core facts.
func detailsReaderFromRegistry(registry *providerapi.ProviderRegistry) componentDetailsReader {
	provider, ok := registry.Get(nicoprovider.ProviderName).(*nicoprovider.Provider)
	if !ok || provider == nil {
		return nil
	}
	return provider.Client()
}

// supportsComponentDetails identifies inventory kinds that Core can read. A missing
// RPC or reader is unavailable evidence for these kinds, not unsupported hardware.
func supportsComponentDetails(kind devicetypes.ComponentType) bool {
	switch kind {
	case devicetypes.ComponentTypeCompute, devicetypes.ComponentTypeNVSwitch, devicetypes.ComponentTypePowerShelf:
		return true
	default:
		return false
	}
}

// detailsReadContext leaves response assembly time inside the enclosing
// proxy call while sharing one budget across inventory and dependency reads.
func detailsReadContext(ctx context.Context) (context.Context, context.CancelFunc) {
	deadline := time.Now().Add(detailsReadTimeout)
	callerDeadline, hasDeadline := ctx.Deadline()
	if hasDeadline && callerDeadline.Add(-detailsResponseReserve).Before(deadline) {
		deadline = callerDeadline.Add(-detailsResponseReserve)
	}
	return context.WithDeadline(ctx, deadline)
}

// sameCoreComponentIdentity accepts Core's canonical lowercase hardware hash while
// preserving its seven-byte typed prefix. Flow can store an uppercase hash from
// ingestion; Core parses that Base32Hex suffix case-insensitively. Exact matches
// also support identifiers outside this 59-byte representation.
func sameCoreComponentIdentity(expected, observed string) bool {
	if expected == observed {
		return true
	}
	if len(expected) != coreComponentIDLength || len(observed) != coreComponentIDLength || expected[:coreComponentIDPrefixLength] != observed[:coreComponentIDPrefixLength] {
		return false
	}
	hash, valid := normalizeCoreComponentHash(expected[coreComponentIDPrefixLength:])
	return valid && hash == strings.ToLower(observed[coreComponentIDPrefixLength:])
}

// normalizeCoreComponentHash checks Core's Base32Hex representation before folding
// hash case. The round trip rejects unused trailing bits that Go discards.
func normalizeCoreComponentHash(hash string) (string, bool) {
	hash = strings.ToUpper(hash)
	encoding := base32.HexEncoding.WithPadding(base32.NoPadding)
	decoded, err := encoding.DecodeString(hash)
	if err != nil || len(decoded) != coreComponentHashBytes || encoding.EncodeToString(decoded) != hash {
		return "", false
	}
	return strings.ToLower(hash), true
}

// normalizeCoreComponentTarget rejects IDs that Core's typed request decoder would
// reject for this component kind. Flow ingestion accepts arbitrary strings, so
// one invalid inventory link must not prevent healthy neighbors from being read.
// Keep this aligned with crates/uuid's machine/host.rs, switch/mod.rs and
// power_shelf/mod.rs FromStr implementations, including predicted hosts and
// host-type power shelves.
func normalizeCoreComponentTarget(kind devicetypes.ComponentType, id string) (string, bool) {
	if len(id) != coreComponentIDLength || (id[6] != 't' && id[6] != 's') {
		return "", false
	}
	validKind := false
	switch kind {
	case devicetypes.ComponentTypeCompute:
		validKind = id[:5] == "fm100" && (id[5] == 'h' || id[5] == 'p')
	case devicetypes.ComponentTypeNVSwitch:
		validKind = id[:5] == "sw100" && id[5] == 'n'
	case devicetypes.ComponentTypePowerShelf:
		validKind = id[:5] == "ps100" && (id[5] == 'r' || id[5] == 'h')
	}
	if !validKind {
		return "", false
	}
	hash, valid := normalizeCoreComponentHash(id[coreComponentIDPrefixLength:])
	if !valid {
		return "", false
	}
	return id[:coreComponentIDPrefixLength] + hash, true
}

// detailsResponseFits measures the actual proxy wrapper, whose JSON
// escaping can be larger than the embedded protobuf JSON alone.
func detailsResponseFits(response proto.Message) (bool, error) {
	message, err := protojson.Marshal(response)
	if err != nil {
		return false, status.Error(codes.Internal, "component details could not be encoded")
	}
	encoded, err := json.Marshal(grpcproxy.Response{ResponseJSON: message})
	if err != nil {
		return false, status.Error(codes.Internal, "component details could not be encoded")
	}
	return len(encoded) <= detailsResponseBytes, nil
}
