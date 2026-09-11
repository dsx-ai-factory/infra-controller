// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sitemgr

import (
	"fmt"
	"net/http"

	"github.com/getsentry/sentry-go"
	"github.com/sirupsen/logrus"
)

// Implements http.Handler
type sentryWrap struct {
	h http.Handler
}

// ServeHTTP method of http.Handler
func (s *sentryWrap) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	sw := &sentryWrapRW{w: w, r: r}
	s.h.ServeHTTP(sw, r)
}

// Implements http.ResponseWriter
type sentryWrapRW struct {
	w http.ResponseWriter
	r *http.Request
}

// WriteHeader overridden for capturing errors
func (rw *sentryWrapRW) WriteHeader(statusCode int) {
	if statusCode >= http.StatusMultipleChoices {
		var url string
		if rw.r.URL != nil {
			url = rw.r.URL.String()
		}
		logrus.Errorf("Sentry reporting error %d for %s", statusCode, url)
		sentry.CaptureException(fmt.Errorf("error %d for %s", statusCode, url))
	}

	rw.w.WriteHeader(statusCode)

}

// Header invokes the inner function
func (rw *sentryWrapRW) Header() http.Header {
	return rw.w.Header()
}

// Write invokes the inner function
func (rw *sentryWrapRW) Write(b []byte) (int, error) {
	return rw.w.Write(b)
}
