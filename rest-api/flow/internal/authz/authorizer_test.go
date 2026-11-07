// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package authz

import (
	"bytes"
	"context"
	"crypto/tls"
	"crypto/x509"
	"net"
	"net/url"
	"testing"

	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/peer"
	"google.golang.org/grpc/status"
)

const (
	allowedIdentity = "spiffe://example.test/ns/site/sa/site-workflow"
	deniedIdentity  = "spiffe://example.test/ns/site/sa/unrelated-service"
)

func TestConfigValidate(t *testing.T) {
	tests := map[string]struct {
		identities []string
		wantErr    string
	}{
		"valid": {
			identities: []string{allowedIdentity},
		},
		"empty": {
			wantErr: "at least one allowed service identity is required",
		},
		"audit may use an empty allowlist": {
			identities: nil,
		},
		"duplicate": {
			identities: []string{allowedIdentity, allowedIdentity},
			wantErr:    "duplicate allowed service identity",
		},
		"wrong scheme": {
			identities: []string{"https://example.test/workload"},
			wantErr:    "not a valid SPIFFE ID",
		},
		"missing trust domain": {
			identities: []string{"spiffe:///ns/site/sa/site-workflow"},
			wantErr:    "not a valid SPIFFE ID",
		},
		"missing workload path": {
			identities: []string{"spiffe://example.test"},
			wantErr:    "must include a workload path",
		},
		"wildcard": {
			identities: []string{"spiffe://example.test/ns/site/sa/*"},
			wantErr:    "not a valid SPIFFE ID",
		},
		"query": {
			identities: []string{"spiffe://example.test/workload?role=admin"},
			wantErr:    "not a valid SPIFFE ID",
		},
		"fragment": {
			identities: []string{"spiffe://example.test/workload#admin"},
			wantErr:    "not a valid SPIFFE ID",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			mode := ModeEnforce
			if name == "audit may use an empty allowlist" {
				mode = ModeAudit
			}
			err := (Config{AllowedServiceIdentities: test.identities, Mode: mode}).Validate()
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestConfigValidateMode(t *testing.T) {
	require.ErrorContains(t, (Config{Mode: "audit"}).Validate(), "authorization mode")
	require.NoError(t, (Config{Mode: ModeAudit}).Validate())
	require.ErrorContains(t, (Config{}).Validate(), "authorization mode")
}

func TestNewMode(t *testing.T) {
	tests := map[string]struct {
		value   string
		want    Mode
		wantErr string
	}{
		"undefined": {
			want: DefaultMode,
		},
		"whitespace uses default": {
			value: "   ",
			want:  DefaultMode,
		},
		"audit": {
			value: string(ModeAudit),
			want:  ModeAudit,
		},
		"audit with whitespace": {
			value: "  " + string(ModeAudit) + "  ",
			want:  ModeAudit,
		},
		"enforce": {
			value: string(ModeEnforce),
			want:  ModeEnforce,
		},
		"invalid": {
			value:   "audit",
			want:    ModeUndefined,
			wantErr: "authorization mode",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			mode, err := NewMode(test.value)
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
			} else {
				require.NoError(t, err)
			}
			assert.Equal(t, test.want, mode)
		})
	}
}

func TestAuthorizerAuthorize(t *testing.T) {
	authorizer, err := New(Config{
		AllowedServiceIdentities: []string{allowedIdentity},
		Mode:                     ModeEnforce,
	})
	require.NoError(t, err)

	tests := map[string]struct {
		context  func() context.Context
		wantCode codes.Code
		wantID   string
	}{
		"missing peer": {
			context:  context.Background,
			wantCode: codes.Unauthenticated,
		},
		"non-TLS peer": {
			context: func() context.Context {
				return peer.NewContext(context.Background(), &peer.Peer{
					AuthInfo: testAuthInfo{},
				})
			},
			wantCode: codes.Unauthenticated,
		},
		"missing verified chain": {
			context: func() context.Context {
				return tlsPeerContext(nil)
			},
			wantCode: codes.Unauthenticated,
		},
		"missing SPIFFE identity": {
			context: func() context.Context {
				return tlsPeerContext(&x509.Certificate{})
			},
			wantCode: codes.Unauthenticated,
		},
		"pathless SPIFFE identity is not allowed": {
			context: func() context.Context {
				return tlsPeerContext(certificateWithURIs(t, "spiffe://example.test"))
			},
			wantCode: codes.PermissionDenied,
		},
		"ambiguous SPIFFE identity": {
			context: func() context.Context {
				return tlsPeerContext(certificateWithURIs(t, allowedIdentity, deniedIdentity))
			},
			wantCode: codes.Unauthenticated,
		},
		"identity is not allowed": {
			context: func() context.Context {
				return tlsPeerContext(certificateWithURIs(t, deniedIdentity))
			},
			wantCode: codes.PermissionDenied,
		},
		"identity is allowed": {
			context: func() context.Context {
				return tlsPeerContext(certificateWithURIs(t, allowedIdentity))
			},
			wantID: allowedIdentity,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			authorization, err := authorizer.authorize(test.context(), "/flow.v1.Flow/Test")
			assert.Equal(t, test.wantCode, status.Code(err))
			assert.Equal(t, test.wantID, authorization.ServiceIdentity)
			assert.Equal(t, test.wantCode == codes.OK, authorization.Allowed)
		})
	}
}

func TestAuditModeLogging(t *testing.T) {
	t.Run("empty allowlist logs once at construction", func(t *testing.T) {
		var output bytes.Buffer
		setTestLogger(t, &output)

		authorizer, err := New(Config{Mode: ModeAudit})
		require.NoError(t, err)
		assert.Contains(t, output.String(), "gRPC authorization allowlist is empty")

		output.Reset()
		_, err = authorizer.authorize(context.Background(), "/flow.v1.Flow/Test")
		require.NoError(t, err)
		assert.Empty(t, output.String())
	})

	t.Run("configured allowlist logs failed authorization", func(t *testing.T) {
		var output bytes.Buffer
		setTestLogger(t, &output)

		authorizer, err := New(Config{
			AllowedServiceIdentities: []string{allowedIdentity},
			Mode:                     ModeAudit,
		})
		require.NoError(t, err)

		_, err = authorizer.authorize(context.Background(), "/flow.v1.Flow/Test")
		require.NoError(t, err)
		assert.Contains(t, output.String(), "gRPC authorization rejected caller")
	})
}

func setTestLogger(t *testing.T, output *bytes.Buffer) {
	t.Helper()
	original := log.Logger
	log.Logger = zerolog.New(output)
	t.Cleanup(func() {
		log.Logger = original
	})
}

type testAuthInfo struct{}

func (testAuthInfo) AuthType() string {
	return "test"
}

func tlsPeerContext(certificate *x509.Certificate) context.Context {
	state := tls.ConnectionState{}
	if certificate != nil {
		state.PeerCertificates = []*x509.Certificate{certificate}
		state.VerifiedChains = [][]*x509.Certificate{{certificate}}
	}

	return peer.NewContext(context.Background(), &peer.Peer{
		Addr:     &net.IPAddr{},
		AuthInfo: credentials.TLSInfo{State: state},
	})
}

func certificateWithURIs(t *testing.T, identities ...string) *x509.Certificate {
	t.Helper()

	certificate := &x509.Certificate{}
	for _, identity := range identities {
		uri, err := url.Parse(identity)
		require.NoError(t, err)
		certificate.URIs = append(certificate.URIs, uri)
	}
	return certificate
}
