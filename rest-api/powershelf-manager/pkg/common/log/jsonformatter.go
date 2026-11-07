// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package log

import (
	"bytes"
	"encoding/json"
	"fmt"
	"path/filepath"
	"runtime"
	"strings"

	log "github.com/sirupsen/logrus"
)

// JSONFormatter is used to log messages in the standard format.
type JSONFormatter struct {
	WithCallDepth int // if > 0 it will be used to find in the call stack the caller to the logging function
}

// Format is the standard formatter for PSM log messages
func (f *JSONFormatter) Format(entry *log.Entry) ([]byte, error) {
	// This is "yyyy-mm-dd HH:MM:SS.000000" TZ format
	const layout = "2006-01-02 15:04:05.000000 MST"

	b := bytes.Buffer{}

	b.WriteString(entry.Time.Format(layout))

	b.WriteString(" [")
	b.WriteString(strings.ToUpper(entry.Level.String()))
	b.WriteString("] ")

	if entry.HasCaller() {
		var filename string
		var line int
		if f.WithCallDepth == 0 { // normal logging
			filename = filepath.Base(entry.Caller.File)
			line = entry.Caller.Line
		} else { // use the filename which made the call to the logger at depth f.WithCallDepth
			var ok bool
			var file string
			_, file, line, ok = runtime.Caller(f.WithCallDepth)
			if !ok {
				file = "???"
				line = 0
			}
			filename = filepath.Base(file)
		}
		lineStr := fmt.Sprintf("%s:%d ", filename, line)
		b.WriteString(lineStr)
	}

	b.WriteString(entry.Message)

	if len(entry.Data) != 0 {
		data, err := json.Marshal(entry.Data)
		if err != nil {
			return nil, fmt.Errorf("failed to marshal fields to JSON, %w", err)
		}

		b.WriteString(" ")
		b.WriteString(string(data))
	}

	b.WriteByte('\n')

	return b.Bytes(), nil
}
