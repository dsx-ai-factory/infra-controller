// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package certmanager

import (
	"bytes"
	"encoding/json"
	"testing"

	"github.com/rs/zerolog"
	"github.com/sirupsen/logrus"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestTLSLogHook_Fire(t *testing.T) {
	for _, tc := range []struct {
		name      string
		level     logrus.Level
		threshold zerolog.Level
		wantLevel string
	}{
		{name: "renewal information", level: logrus.InfoLevel, wantLevel: "info"},
		{name: "CA change warning", level: logrus.WarnLevel, wantLevel: "warn"},
		{name: "reload error", level: logrus.ErrorLevel, wantLevel: "error"},
		{name: "service threshold suppresses information", level: logrus.InfoLevel, threshold: zerolog.ErrorLevel},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var logs bytes.Buffer
			hook := tlsLogHook{logger: zerolog.New(&logs).Level(tc.threshold).With().Str("component", "site-manager").Logger()}
			require.Contains(t, hook.Levels(), tc.level)
			require.NoError(t, hook.Fire(&logrus.Entry{Level: tc.level, Message: "TLS diagnostic", Data: logrus.Fields{"path": "ca.crt"}}))
			if tc.wantLevel == "" {
				assert.Empty(t, logs.String())
				return
			}
			var record map[string]any
			require.NoError(t, json.Unmarshal(logs.Bytes(), &record))
			assert.Equal(t, tc.wantLevel, record[zerolog.LevelFieldName])
			assert.Equal(t, "TLS diagnostic", record[zerolog.MessageFieldName])
			assert.Equal(t, "site-manager", record["component"])
			assert.Equal(t, "ca.crt", record["path"])
		})
	}
}
