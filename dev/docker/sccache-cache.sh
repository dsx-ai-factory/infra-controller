#!/bin/sh
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Cache-backend selection for the Rust builder stages.
#
# Sourced rather than executed, so that the variables sccache_setup exports
# reach the cargo invocation that follows it:
#
#   . /carbide/dev/docker/sccache-cache.sh && \
#     sccache_setup && \
#     cargo build ... && \
#     sccache_report
#
# POSIX sh only -- a Dockerfile RUN is interpreted by /bin/sh.

# One counter from `sccache --show-stats`, or "unknown" when it cannot be read.
# $1 is the JSON field, $2 the label in the human-readable output. The text
# output is parsed as a fallback because --stats-format is the newer of the two
# interfaces, and it is the only source for counters that JSON reports as a
# per-language object rather than a number.
sccache_stat() {
	_stat="$(sccache --show-stats --stats-format=json 2>/dev/null |
		grep -oE "\"$1\" *: *[0-9]+" |
		head -n 1 |
		grep -oE '[0-9]+$')"
	if [ -z "${_stat}" ]; then
		_stat="$(sccache --show-stats 2>/dev/null |
			awk -v label="$2" 'index($0, label) == 1 { print $NF; exit }')"
	fi
	case "${_stat}" in
	'' | *[!0-9]*) echo unknown ;;
	*) echo "${_stat}" ;;
	esac
}

# Number of backend writes sccache has failed so far, or "unknown" when it
# cannot be determined.
sccache_write_errors() {
	sccache_stat cache_write_errors 'Cache write errors'
}

sccache_dump_log() {
	[ -n "${SCCACHE_ERROR_LOG}" ] && [ -s "${SCCACHE_ERROR_LOG}" ] || return 0
	echo "--- sccache server log (deduplicated, last 40 lines) ---"
	# A failing backend produces one near-identical line per compilation, so
	# thousands of them collapse to a handful of distinct messages.
	sort -u "${SCCACHE_ERROR_LOG}" | tail -n 40
	echo "--- end sccache server log ---"
	# Truncated so that a later dump reports only what happened since, rather
	# than reprinting the setup-time failure.
	: >"${SCCACHE_ERROR_LOG}"
	return 0
}

# A single greppable marker for both the "never worked" and the "stopped
# working partway through" case.
sccache_degraded_banner() {
	echo "=============================================================="
	echo "SCCACHE_BACKEND_DEGRADED: $1"
	echo "Compiled output is not being shared with the next build."
	echo "=============================================================="
	return 0
}

# Whether sccache's own startup write check failed, leaving the server unable
# to write for the rest of its life. `RemoteStorage::check` performs a single
# write when the server starts and, on any error at all (including a
# transient rate limit), wraps the storage in `ReadOnlyStorage` until the
# process exits. The message it prints is the only way to tell that apart from
# an individual write losing a race with the rate limiter.
sccache_startup_check_failed() {
	[ -n "${SCCACHE_ERROR_LOG}" ] && [ -s "${SCCACHE_ERROR_LOG}" ] || return 1
	grep -q 'storage write check failed' "${SCCACHE_ERROR_LOG}"
}

# A probe crate whose contents have never been compiled before. The nonce is
# what makes the check below meaningful: with fixed contents the probe's own
# artifact ends up in the cache, and from then on every probe is a read hit
# that attempts no write, records no write error, and so reports a server that
# cannot write anything as healthy.
sccache_probe_source() {
	_nonce="$(od -An -tx4 -N16 /dev/urandom 2>/dev/null | tr -d ' \n')"
	[ -n "${_nonce}" ] || _nonce="$$$(date +%s)"
	printf 'pub const PROBE: &str = "%s";\n' "${_nonce}"
}

# Whether the configured backend actually stores anything. `sccache
# --start-server` only proves the local daemon came up; a server that starts
# cleanly can still silently discard every artifact for the rest of the build.
# The only reliable check is to compile something and look at the resulting
# write-error count.
sccache_backend_writable() {
	# Cleared first because the returns below leave it untouched. A stale
	# count from a previous attempt would otherwise read as a probe that ran
	# and found the backend healthy, when no probe ran at all.
	SCCACHE_PROBE_WRITE_ERRORS=unknown
	_probe_dir="$(mktemp -d)" || return 1
	sccache_probe_source >"${_probe_dir}/probe.rs"
	# Deliberately an ordinary rlib compile. sccache only writes to the
	# backend for invocations it considers cacheable, so a probe using an
	# exotic --emit would pass without exercising a write.
	sccache rustc --crate-name sccache_probe --crate-type lib \
		"${_probe_dir}/probe.rs" --out-dir "${_probe_dir}" >/dev/null 2>&1
	_probe_rc=$?
	rm -rf "${_probe_dir}"
	[ "${_probe_rc}" -eq 0 ] || return 1
	# The server stores the result after answering the client, so the
	# counter is not necessarily up to date the instant the compile returns.
	sleep 3
	SCCACHE_PROBE_WRITE_ERRORS="$(sccache_write_errors)"
	[ "${SCCACHE_PROBE_WRITE_ERRORS}" = 0 ]
}

sccache_setup() {
	SCCACHE_ERROR_LOG=/tmp/sccache-server.log
	SCCACHE_LOG=warn
	SCCACHE_PROBE_WRITE_ERRORS=unknown
	SCCACHE_BACKEND_READ_ONLY=0
	export SCCACHE_ERROR_LOG SCCACHE_LOG

	# No credentials: a plain local `docker build`, where the BuildKit cache
	# mount is the only backend available.
	if ! grep -qE "^https://" /run/secrets/actions_results_url 2>/dev/null; then
		echo "sccache: using local cache mount at ${SCCACHE_DIR}"
		sccache --start-server >/dev/null 2>&1 || true
		return 0
	fi

	SCCACHE_GHA_ENABLED=true
	ACTIONS_RESULTS_URL="$(cat /run/secrets/actions_results_url)"
	ACTIONS_RUNTIME_TOKEN="$(cat /run/secrets/actions_runtime_token)"
	SCCACHE_GHA_RW_MODE="$(cat /run/secrets/sccache_gha_rw_mode 2>/dev/null || true)"
	case "${SCCACHE_GHA_RW_MODE}" in
	READ_ONLY | READ_WRITE) ;;
	*)
		echo "sccache: missing or invalid GHA access mode; defaulting to READ_ONLY"
		SCCACHE_GHA_RW_MODE=READ_ONLY
		;;
	esac

	# sccache 0.17 uses OpenDAL 0.55, which defaults to the retired v1
	# artifact-cache API unless this flag is present. ACTIONS_RESULTS_URL is
	# the v2 endpoint; combining it with v1 paths makes every lookup and write
	# fail with 404.
	ACTIONS_CACHE_SERVICE_V2=true
	export SCCACHE_GHA_ENABLED ACTIONS_RESULTS_URL ACTIONS_RUNTIME_TOKEN
	export SCCACHE_GHA_RW_MODE ACTIONS_CACHE_SERVICE_V2

	# Retried because the decision is made once and is unrecoverable: a
	# single rate-limited call during the server's startup write check
	# disables writes for as long as that server lives, which on a container
	# build means every artifact of a twenty-minute compile is discarded. A
	# fresh server runs a fresh check, so the cost of being wrong here is far
	# higher than the cost of a few restarts.
	_attempt=1
	while :; do
		# Emptied so that the classification below reads only the server
		# started on this iteration. The log path is fixed, and a Docker
		# build reuses /tmp across RUN layers, so anything left behind by
		# an earlier server would otherwise be attributed to this one.
		: >"${SCCACHE_ERROR_LOG}"
		if ! sccache --start-server >/dev/null 2>&1; then
			_reason="the sccache server would not start"
			break
		fi

		if [ "${SCCACHE_GHA_RW_MODE}" = READ_ONLY ]; then
			echo "sccache: using GitHub Actions cache backend (read-only)"
			SCCACHE_BACKEND_READ_ONLY=1
			return 0
		fi

		if sccache_backend_writable; then
			echo "sccache: using GitHub Actions cache backend"
			return 0
		fi

		if [ "${SCCACHE_PROBE_WRITE_ERRORS}" = unknown ]; then
			_reason="the Actions cache write probe statistics were unavailable"
			break
		fi

		# Checked before the log is dumped, because dumping truncates it.
		if ! sccache_startup_check_failed; then
			# The server can write; this one probe was throttled. That
			# costs a single artifact, so replacing a shared backend
			# over it would be a much worse trade.
			sccache_dump_log
			echo "sccache: using GitHub Actions cache backend (the write probe was rate-limited, but the backend accepts writes)"
			return 0
		fi

		sccache_dump_log
		if [ "${_attempt}" -ge 3 ]; then
			_reason="the Actions cache rejected the startup write check on ${_attempt} successive servers"
			break
		fi
		echo "sccache: the Actions cache rejected the startup write check (attempt ${_attempt}); restarting the server"
		# A restart is required, not just a reconfigure: read-only mode is
		# held by the running server and survives any change to the
		# environment.
		sccache --stop-server >/dev/null 2>&1 || true
		sleep "$((_attempt * 5))"
		_attempt="$((_attempt + 1))"
	done

	sccache_dump_log
	sccache_degraded_banner "${_reason}; falling back to the local cache mount"
	sccache --stop-server >/dev/null 2>&1 || true
	unset SCCACHE_GHA_ENABLED ACTIONS_RESULTS_URL ACTIONS_RUNTIME_TOKEN
	unset SCCACHE_GHA_RW_MODE ACTIONS_CACHE_SERVICE_V2
	sccache --start-server >/dev/null 2>&1 || true
	return 0
}

# `sccache --show-stats` with the write-error row relabelled. sccache reports
# every declined write as an error, which reads as a broken backend to anyone
# scanning the log, so the row is renamed to what it actually counts here. The
# figure itself is left alone, and the row keeps its width so the table stays
# aligned.
sccache_show_stats_read_only() {
	sccache --show-stats 2>&1 | awk '
		index($0, "Cache write errors") == 1 {
			label = "Cache writes skipped (read-only)"
			pad = length($0) - length(label) - length($NF)
			if (pad < 1) {
				pad = 1
			}
			printf "%s%*s\n", label, pad + length($NF), $NF
			next
		}
		{ print }
	'
	return 0
}

# Health of a deliberately read-only backend. sccache has no "do not attempt
# the write" mode: READ_ONLY is implemented by failing every put, and each
# rejection lands in the write-error counter, so that counter is guaranteed to
# equal the number of cacheable compilations and says nothing about the
# backend. Reads are the only signal. Misses are logged as NotFound warnings,
# which makes the server log pure noise unless a read actually errored.
sccache_report_read_only() {
	sccache_show_stats_read_only
	echo "sccache: writes are declined on this ref by design -- only main publishes to the shared cache."
	_read_errors="$(sccache_stat cache_read_errors 'Cache read errors')"
	case "${_read_errors}" in
	0) ;;
	unknown)
		sccache_dump_log
		sccache_degraded_banner "cache statistics unavailable after the build"
		return 0
		;;
	*)
		sccache_dump_log
		sccache_degraded_banner "${_read_errors} cache read errors during the build"
		return 0
		;;
	esac
	if [ "$(sccache_stat cache_hits 'Cache hits')" = 0 ]; then
		echo "sccache: the backend was reachable but held nothing matching this build, so every crate was compiled from scratch."
	fi
	return 0
}

sccache_report() {
	if [ "${SCCACHE_BACKEND_READ_ONLY}" = 1 ]; then
		sccache_report_read_only
		return 0
	fi
	sccache --show-stats || true
	sccache_dump_log
	_errors="$(sccache_write_errors)"
	if [ "${_errors}" = unknown ]; then
		sccache_degraded_banner "cache write statistics unavailable after the build"
	elif [ "${_errors}" -gt 0 ]; then
		sccache_degraded_banner "${_errors} cache write errors during the build"
	fi
	return 0
}
