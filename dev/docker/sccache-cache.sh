#!/bin/sh
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# sccache setup and reporting for the Rust builder stages and build containers.
#
# Sourced rather than executed, so that the variables sccache_setup exports
# reach the cargo invocation that follows it:
#
#   . /carbide/dev/docker/sccache-cache.sh && \
#     sccache_setup && \
#     cargo build ... && \
#     sccache_report
#
# POSIX sh only, because a Dockerfile RUN is interpreted by /bin/sh.
#
# sccache always uses the local directory in SCCACHE_DIR. CI restores that
# directory from the Actions cache before the build and saves it again on main,
# which costs two cache requests per build. Using the Actions cache as sccache's
# own backend cost one request per compilation and held the repository at the
# cache API rate limit.

# Copies SCCACHE_SEED_DIR into SCCACHE_DIR when it holds anything. The Docker
# builds mount the restored directory there read-only, and sccache needs a
# writable store, so the seed is copied into the BuildKit cache mount.
sccache_seed() {
	[ -n "${SCCACHE_SEED_DIR:-}" ] || return 0
	[ -n "$(ls -A "${SCCACHE_SEED_DIR}" 2>/dev/null)" ] || {
		echo "sccache: no restored cache in ${SCCACHE_SEED_DIR}; starting empty"
		return 0
	}
	mkdir -p "${SCCACHE_DIR}"
	cp -a "${SCCACHE_SEED_DIR}/." "${SCCACHE_DIR}/"
	echo "sccache: seeded ${SCCACHE_DIR} from ${SCCACHE_SEED_DIR} ($(du -sh "${SCCACHE_DIR}" | cut -f1))"
}

sccache_setup() {
	# Bounds the directory CI saves, matching test-release-container-services.
	: "${SCCACHE_CACHE_SIZE:=5G}"
	# The server runs detached, so without a log file the reason behind a
	# write error is discarded and only the count shows up in the stats.
	# sccache logs disk write errors at debug level, so sccache::server is
	# raised to debug.
	SCCACHE_ERROR_LOG=/tmp/sccache-server.log
	SCCACHE_LOG='warn,sccache::server=debug'
	export SCCACHE_CACHE_SIZE SCCACHE_ERROR_LOG SCCACHE_LOG
	: >"${SCCACHE_ERROR_LOG}"
	sccache_seed
	echo "sccache: using local cache directory ${SCCACHE_DIR}"
	sccache --start-server >/dev/null 2>&1 || true
	return 0
}

sccache_report() {
	_stats="$(sccache --show-stats 2>&1)" || true
	printf '%s\n' "${_stats}"
	echo "sccache: ${SCCACHE_DIR} holds $(du -sh "${SCCACHE_DIR}" 2>/dev/null | cut -f1)"
	_write_errors="$(printf '%s\n' "${_stats}" |
		awk 'index($0, "Cache write errors") == 1 { print $NF; exit }')"
	if [ "${_write_errors}" != 0 ] && [ -s "${SCCACHE_ERROR_LOG}" ]; then
		echo "--- sccache server errors (last 20) ---"
		grep -E ' (WARN|ERROR) |Error executing cache write' "${SCCACHE_ERROR_LOG}" | tail -n 20
		echo "--- end sccache server errors ---"
	fi
	# Removed so the per-compile debug lines don't stay in the builder layer.
	rm -f "${SCCACHE_ERROR_LOG}"
	return 0
}
