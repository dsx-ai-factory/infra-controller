// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package core

import (
	"context"
	"syscall"
	"testing"
	"time"

	"github.com/sirupsen/logrus"
	"github.com/stretchr/testify/assert"
)

type mockDaemon struct{}

func (d *mockDaemon) Start(ctx context.Context) {
	log := GetLogger(ctx)

	go func() {
		ticker := time.NewTicker(1 * time.Second)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case ts := <-ticker.C:
				log.Infof("Receive ticker: %v", ts)

				newCtx := WithLogger(ctx, log.WithField("foo", "bar"))
				go d.computeInBackground(newCtx)
			}
		}
	}()
}

func (d *mockDaemon) computeInBackground(ctx context.Context) {
	log := GetLogger(ctx)

	select {
	case <-ctx.Done():
		return
	default:
		log.Infof("Computation in background")
	}
}

func TestDaemon(t *testing.T) {
	ctx := context.Background()
	GetLogger(ctx).Infof("This is noop logger")

	//code coverage Fix , call with invalid context
	appname := GetAppName(ctx)
	if len(appname) == 0 {
		GetLogger(ctx).Infof("appName is empty invoke it with correct object")
	}

	ctx = NewDefaultContext(ctx)
	ctx, hook := WithTestingLogger(ctx)

	GetLogger(ctx).Infof("appName: %s", GetAppName(ctx))

	d := &mockDaemon{}
	d.Start(ctx)

	time.Sleep(2 * time.Second)
	assert.NoError(t, syscall.Kill(syscall.Getpid(), syscall.SIGTERM))

	<-ctx.Done()
	SetDefaultLogLevel(logrus.InfoLevel)
	//code coverage and setLevel  for empty string and check if its successful
	assert.Error(t, SetLevel(GetLogger(ctx), ""), "SetLevel failed for empty string")

	assert.NoError(t, SetLevel(GetLogger(ctx), "info"))
	GetLogger(ctx).Infof("ctx.Done(), shutting down")

	// GetCurrentTime
	GetLogger(ctx).Infof("GetCurrentTime(), %v", GetCurrentTime(ctx))
	ctx = WithClock(ctx, func() time.Time {
		t, _ := time.Parse(time.RFC3339, "2020-10-26T21:48:00-07:00")
		return t
	})
	assert.Equal(t, int64(1603774080), GetCurrentTime(ctx).Unix())

	log := GetLogger(WithDefaultLogger(ctx))
	for i, e := range hook.AllEntries() {
		log.Infof("entries[%d]: %s, fields: %+v", i, e.Message, e.Data)
	}

	// Test default pseudo-random number generators (seed = 0)
	assert.Equal(t, 74, GetRandIntn(context.Background(), 100))
	assert.Equal(t, float64(0.24496508529377975), GetRandFloat64(context.Background()))
	assert.Equal(t, []byte{0x73, 0xc8, 0x6e}, GetRandomBytes(context.Background(), 3))

	// Test pseudo-random number generators WithRandomSeed 42
	assert.Equal(t, 5, GetRandIntn(WithRandomSeed(context.Background(), 42), 100))
	assert.Equal(t, float64(0.3730283610466326), GetRandFloat64(WithRandomSeed(context.Background(), 42)))
	assert.Equal(t, []byte{0x53, 0x8c, 0x7f}, GetRandomBytes(WithRandomSeed(context.Background(), 42), 3))
}

func TestHelpers(t *testing.T) {
	list := []string{"one", "two", "three", "four"}

	assert.Equal(t, true, ContainsString(list, "one"))
	assert.Equal(t, false, ContainsString(list, "five"))

	list = RemoveString(list, "one")
	assert.Equal(t, false, ContainsString(list, "one"))
	assert.Equal(t, true, ContainsString(list, "two"))
}
