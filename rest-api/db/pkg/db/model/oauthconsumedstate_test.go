// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"crypto/sha256"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestNewOAuthConsumedStateDAO(t *testing.T) {
	dbSession := testInitDB(t)
	defer dbSession.Close()

	tests := []struct {
		name string
	}{
		{
			name: "initializes OAuthConsumedStateSQLDAO",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ocssd := NewOAuthConsumedStateDAO(dbSession)
			assert.NotNil(t, ocssd)
		})
	}
}

func TestOAuthConsumedStateSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()

	err := dbSession.DB.ResetModel(ctx, (*OAuthConsumedState)(nil))
	require.NoError(t, err)

	ocssd := NewOAuthConsumedStateDAO(dbSession)
	now := time.Now().UTC().Truncate(time.Second)
	sharedDigest := sha256.Sum256([]byte("state-one"))
	concurrentDigest := sha256.Sum256([]byte("state-concurrent"))

	tests := []struct {
		name        string
		input       OAuthConsumedStateCreateInput
		workers     int
		wantCreated bool
		wantWinners int32
		wantErr     bool
	}{
		{
			name: "creates new consumed state digest",
			input: OAuthConsumedStateCreateInput{
				StateDigest: sharedDigest[:],
				ExpiresAt:   now.Add(10 * time.Minute),
			},
			wantCreated: true,
		},
		{
			name: "rejects duplicate consumed state digest",
			input: OAuthConsumedStateCreateInput{
				StateDigest: sharedDigest[:],
				ExpiresAt:   now.Add(10 * time.Minute),
			},
			wantCreated: false,
		},
		{
			name: "rejects digest shorter than 32 bytes",
			input: OAuthConsumedStateCreateInput{
				StateDigest: []byte("short"),
				ExpiresAt:   now.Add(10 * time.Minute),
			},
			wantErr: true,
		},
		{
			name: "concurrent callers elect one winner",
			input: OAuthConsumedStateCreateInput{
				StateDigest: concurrentDigest[:],
				ExpiresAt:   now.Add(10 * time.Minute),
			},
			workers:     12,
			wantWinners: 1,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if tt.workers > 0 {
				start := make(chan struct{})
				var wg sync.WaitGroup
				var winners atomic.Int32

				for range tt.workers {
					wg.Add(1)
					go func() {
						defer wg.Done()
						<-start
						_, created, cerr := ocssd.Create(ctx, nil, tt.input)
						assert.NoError(t, cerr)
						if created {
							winners.Add(1)
						}
					}()
				}

				close(start)
				wg.Wait()
				assert.Equal(t, tt.wantWinners, winners.Load())
				return
			}

			got, created, cerr := ocssd.Create(ctx, nil, tt.input)
			if tt.wantErr {
				require.Error(t, cerr)
				assert.False(t, created)
				assert.Nil(t, got)
				return
			}
			require.NoError(t, cerr)
			assert.Equal(t, tt.wantCreated, created)
			if tt.wantCreated {
				require.NotNil(t, got)
				assert.Equal(t, tt.input.StateDigest, got.StateDigest)
			} else {
				assert.Nil(t, got)
			}
		})
	}
}

func TestOAuthConsumedStateSQLDAO_DeleteExpired(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()

	ocssd := NewOAuthConsumedStateDAO(dbSession)
	now := time.Now().UTC().Truncate(time.Second)

	tests := []struct {
		name            string
		limit           int
		wantRemaining   int
		wantActiveKept  bool
		wantExpiredKept bool
	}{
		{
			name:            "non-positive limit is a no-op",
			limit:           0,
			wantRemaining:   3,
			wantActiveKept:  true,
			wantExpiredKept: true,
		},
		{
			name:            "bounded limit deletes up to limit expired rows and keeps active rows",
			limit:           1,
			wantRemaining:   2,
			wantActiveKept:  true,
			wantExpiredKept: true,
		},
		{
			name:            "deletes all expired rows within limit and keeps active rows",
			limit:           10,
			wantRemaining:   1,
			wantActiveKept:  true,
			wantExpiredKept: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := dbSession.DB.ResetModel(ctx, (*OAuthConsumedState)(nil))
			require.NoError(t, err)

			expiredOne := sha256.Sum256([]byte("expired-1"))
			expiredTwo := sha256.Sum256([]byte("expired-2"))
			active := sha256.Sum256([]byte("active-1"))

			_, created, err := ocssd.Create(ctx, nil, OAuthConsumedStateCreateInput{
				StateDigest: expiredOne[:],
				ExpiresAt:   now.Add(-10 * time.Minute),
			})
			require.NoError(t, err)
			require.True(t, created)

			_, created, err = ocssd.Create(ctx, nil, OAuthConsumedStateCreateInput{
				StateDigest: expiredTwo[:],
				ExpiresAt:   now.Add(-5 * time.Minute),
			})
			require.NoError(t, err)
			require.True(t, created)

			_, created, err = ocssd.Create(ctx, nil, OAuthConsumedStateCreateInput{
				StateDigest: active[:],
				ExpiresAt:   now.Add(10 * time.Minute),
			})
			require.NoError(t, err)
			require.True(t, created)

			err = ocssd.DeleteExpired(ctx, nil, now.Add(-5*time.Minute), tt.limit)
			require.NoError(t, err)

			total, err := dbSession.DB.NewSelect().Model((*OAuthConsumedState)(nil)).Count(ctx)
			require.NoError(t, err)
			assert.Equal(t, tt.wantRemaining, total)

			activeExists, err := dbSession.DB.NewSelect().Model((*OAuthConsumedState)(nil)).
				Where("state_digest = ?", active[:]).Exists(ctx)
			require.NoError(t, err)
			assert.Equal(t, tt.wantActiveKept, activeExists)

			expiredCount, err := dbSession.DB.NewSelect().Model((*OAuthConsumedState)(nil)).
				Where("expires_at <= ?", now.Add(-5*time.Minute)).Count(ctx)
			require.NoError(t, err)
			assert.Equal(t, tt.wantExpiredKept, expiredCount > 0)
		})
	}
}
