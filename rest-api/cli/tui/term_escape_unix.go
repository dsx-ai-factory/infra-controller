// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//go:build aix || darwin || dragonfly || freebsd || linux || netbsd || openbsd || solaris

package tui

import (
	"errors"
	"fmt"
	"os"
	"time"

	"golang.org/x/sys/unix"
	"golang.org/x/term"
)

const escapeSequenceWait = 50 * time.Millisecond

func terminalEOFIsTransient(input *os.File) (bool, error) {
	if !term.IsTerminal(int(input.Fd())) {
		return false, nil
	}
	pollFDs := []unix.PollFd{{
		Fd:     int32(input.Fd()),
		Events: unix.POLLIN,
	}}
	for {
		_, err := unix.Poll(pollFDs, 0)
		if errors.Is(err, unix.EINTR) {
			continue
		}
		if err != nil {
			return false, fmt.Errorf("polling terminal input: %w", err)
		}
		return pollFDs[0].Revents&(unix.POLLHUP|unix.POLLERR|unix.POLLNVAL) == 0, nil
	}
}

// readEscapeSequence waits briefly for the two bytes that distinguish an
// arrow-key escape sequence from a lone Escape key. Polling avoids blocking
// indefinitely when Escape is used to cancel a selector.
func readEscapeSequence(input *os.File, sequence []byte) (int, error) {
	deadline := time.Now().Add(escapeSequenceWait)
	total := 0
	pollFDs := []unix.PollFd{{
		Fd:     int32(input.Fd()),
		Events: unix.POLLIN,
	}}

	for total < len(sequence) {
		remaining := time.Until(deadline)
		if remaining <= 0 {
			return total, nil
		}
		timeoutMillis := int((remaining + time.Millisecond - 1) / time.Millisecond)
		ready, err := unix.Poll(pollFDs, timeoutMillis)
		if errors.Is(err, unix.EINTR) {
			continue
		}
		if err != nil {
			return total, err
		}
		if ready == 0 {
			return total, nil
		}

		read, err := input.Read(sequence[total:])
		total += read
		if err != nil {
			return total, err
		}
	}
	return total, nil
}
