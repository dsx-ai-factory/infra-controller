#!/bin/bash

# Ensure fd 3 is open — wrapper scripts (provision-dpu.sh, post-power-cycle.sh)
# redirect it to tee(log+tty), but direct invocation or unwrapped sourcing leaves
# it closed. Fall back to stderr so >&3 writes always succeed.
{ true >&3; } 2>/dev/null || exec 3>&2

# SCRIPTS_DIR is this script's own install directory, set up by install.sh.
SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Touchfiles all live under a dedicated subdirectory so they can be cleared atomically.
TOUCHFILES_DIR="${SCRIPTS_DIR}/touchfiles"

TOUCHFILE_BFB_UPDATED="${TOUCHFILES_DIR}/bfbupdated"
TOUCHFILE_HBN_CONFIG_STAGED="${TOUCHFILES_DIR}/hbnconfigstaged"
TOUCHFILE_HBN_SETUP_A="${TOUCHFILES_DIR}/hbnsetupa"
TOUCHFILE_HBN_DEPLOYED="${TOUCHFILES_DIR}/hbndeployed"
TOUCHFILE_NETPLAN_CONFIGURED="${TOUCHFILES_DIR}/netplanconfigured"
CUR_STEP=0
FINAL_STEP=11

STEPS=(
    "Prepare for DPU install"
    "Copy files required for DPU and HBN install"
    "Prepare DPU SSH access"
    "Start rshim service"
    "Stage HBN configuration"
    "Install BFB"
    "Setup tmfifo virtual ethernet interface"
    "Prepare DPU for HBN deployment"
    "HBN deployment"
    "Configure netplan"
    "Power cycle"
    "Verify HBN container"
)

# ── HBN version config ────────────────────────────────────────────────────────

_hbn_versions_cfg="$SCRIPTS_DIR/doca_hbn_versions.cfg"
[[ -f "$_hbn_versions_cfg" ]] || { echo "ERROR: doca_hbn_versions.cfg not found at $_hbn_versions_cfg" >&2; exit 1; }
# shellcheck source=/dev/null
source "$_hbn_versions_cfg"
[[ -z "${HBN_CONFIG_SRC_DIR:-}" ]] && { echo "ERROR: HBN_CONFIG_SRC_DIR not set in $_hbn_versions_cfg" >&2; exit 1; }
[[ -z "${HBN_SCRIPT_DIR:-}" ]]     && { echo "ERROR: HBN_SCRIPT_DIR not set in $_hbn_versions_cfg" >&2; exit 1; }

# ── DPU SSH provisioning ───────────────────────────────────────────────────────

DPU_SSH_USER="${DPU_SSH_USER:-root}"
DPU_SSH_HOST="${DPU_SSH_HOST:-192.168.100.2}"
DPU_REMOTE_DIR="/root/dpucfg"

_dpu_ssh_dir="$SCRIPTS_DIR"
DPU_SSH_KEY_SECURE="/root/.dpu_provision/dpu_provision_ed25519"
DPU_SSH_TOUCHFILE="/var/dpu_ssh_prepared"
DPU_SSH_KEY=""
DPU_BFB_CFG=""
DPU_SSH_OPTS=()

_dpu_ssh_pubkey_placeholder='__DPU_PROVISION_SSH_PUBKEY__'
_dpu_ssh_bf_src="${_dpu_ssh_dir}/bf.cfg"
_dpu_ssh_bf_prepared="/root/.dpu_provision/bf.cfg"
_DPU_SSH_PREPARE_DONE=false

dpu_ssh_prepare() {
	if [ -n "$DPU_SSH_KEY" ] && [ -r "$DPU_SSH_KEY" ]; then
		return 0
	fi

	local verbose=true
	if [ "$_DPU_SSH_PREPARE_DONE" = true ]; then
		verbose=false
	fi

	[ "$verbose" = true ] && update_progress 2
	local key="$DPU_SSH_KEY_SECURE"
	local -a files=("$DPU_SSH_TOUCHFILE" "$key" "${key}.pub" "$_dpu_ssh_bf_prepared")
	local n=0
	for f in "${files[@]}"; do [ -f "$f" ] && n=$(( n + 1 )); done

	if (( n == ${#files[@]} )); then
		if [ "$verbose" = true ]; then
			echo "DPU SSH: all provisioning files present, reusing existing key and bf.cfg."
		fi
		chmod 600 "$key"
		DPU_SSH_KEY="$key"
		DPU_BFB_CFG="$_dpu_ssh_bf_prepared"
		DPU_SSH_OPTS=(-i "$key" -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=/dev/null -o ServerAliveInterval=30 -o ServerAliveCountMax=3)
		export DPU_BFB_CFG
		export DPU_SSH_OPTS
		export DPU_SSH_KEY
		_DPU_SSH_PREPARE_DONE=true
		return 0
	fi

	if (( n > 0 )); then
		echo "ERROR: partial DPU provisioning state — missing:" >&3
		for f in "${files[@]}"; do [ -f "$f" ] || echo "  $f" >&3; done
		return 1
	fi

	# None exist — create from scratch
	if [ "$verbose" = true ]; then
		echo "DPU SSH: no existing provisioning found, creating SSH key and bf.cfg..."
	fi

	if ! command -v ssh-keygen >/dev/null 2>&1; then
		echo "ERROR: ssh-keygen not found; cannot create DPU provision SSH key" >&3
		return 1
	fi
	if [ ! -f "$_dpu_ssh_bf_src" ]; then
		echo "ERROR: bf.cfg template not found at $_dpu_ssh_bf_src" >&3
		return 1
	fi

	install -d -m 700 /root/.dpu_provision
	[ "$verbose" = true ] && echo "DPU SSH: generating ed25519 key at $key"
	ssh-keygen -t ed25519 -N "" -C "dpu-provision@forge" -f "$key" >/dev/null
	chmod 600 "$key"

	[ "$verbose" = true ] && echo "DPU SSH: injecting public key into bf.cfg template"
	local pub_line
	pub_line="$(ssh-keygen -y -f "$key")"
	install -m 600 /dev/null "$_dpu_ssh_bf_prepared"
	sed "s|${_dpu_ssh_pubkey_placeholder}|${pub_line}|g" "$_dpu_ssh_bf_src" > "$_dpu_ssh_bf_prepared"
	if ! grep -qF "$pub_line" "$_dpu_ssh_bf_prepared"; then
		rm -f "$_dpu_ssh_bf_prepared" "$key" "${key}.pub"
		echo "ERROR: bf.cfg: failed to set provision SSH public key" >&3
		return 1
	fi

	touch "$DPU_SSH_TOUCHFILE"
	DPU_SSH_KEY="$key"
	DPU_BFB_CFG="$_dpu_ssh_bf_prepared"
	DPU_SSH_OPTS=(-i "$key" -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=/dev/null -o ServerAliveInterval=30 -o ServerAliveCountMax=3)
	export DPU_SSH_OPTS
	export DPU_SSH_KEY
	export DPU_BFB_CFG
	_DPU_SSH_PREPARE_DONE=true
	if [ "$verbose" = true ]; then
		echo "DPU SSH: provisioning complete. Key: $key  BFB config: $_dpu_ssh_bf_prepared"
	fi
}

_dpu_ssh_require_key() {
	if [ -n "$DPU_SSH_KEY" ] && [ -r "$DPU_SSH_KEY" ]; then
		return 0
	fi
	dpu_ssh_prepare || return 1
	if [ -n "$DPU_SSH_KEY" ] && [ -r "$DPU_SSH_KEY" ]; then
		return 0
	fi
	echo "ERROR: DPU provision SSH key not available" >&3
	return 1
}

_dpu_ssh_run() {
	local host_spec="$1"
	shift
	local status=0

	echo "DPU SSH: running on ${host_spec}: $*" >&2
	_dpu_ssh_require_key
	ssh "${DPU_SSH_OPTS[@]}" "${host_spec}" "$@" || status=$?
	if [ "$status" -ne 0 ]; then
		echo "ERROR: DPU SSH command failed (exit ${status}) on ${host_spec}: $*" >&3
		return "$status"
	fi
	echo "DPU SSH: command completed successfully on ${host_spec}" >&2
	return 0
}

dpu_ssh() {
	_dpu_ssh_run "${DPU_SSH_USER}@${DPU_SSH_HOST}" "$@"
}

dpu_ssh_root() {
	local attempt status=1
	local max_attempts=5
	local retry_interval=20

	echo "DPU SSH (root): running on ${DPU_SSH_HOST}: $*" >&2
	for ((attempt = 1; attempt <= max_attempts; attempt++)); do
		status=0
		_dpu_ssh_run "root@${DPU_SSH_HOST}" "$@" || status=$?
		[ "$status" -eq 0 ] && return 0
		if [ "$attempt" -lt "$max_attempts" ]; then
			echo "DPU SSH as root failed, retrying in ${retry_interval}s (attempt ${attempt}/${max_attempts})..." >&3
			sleep "$retry_interval"
		fi
	done
	echo "ERROR: DPU SSH as root failed after ${max_attempts} attempts: $*" >&3
	return "$status"
}

dpu_scp() {
	local attempt status=1
	local max_attempts=3
	local retry_interval=60

	_dpu_ssh_require_key
	for ((attempt = 1; attempt <= max_attempts; attempt++)); do
		echo "DPU SCP (attempt ${attempt}/${max_attempts}): $*" >&3
		status=0
		scp "${DPU_SSH_OPTS[@]}" "$@" || status=$?
		if [ "$status" -eq 0 ]; then
			echo "DPU SCP: transfer complete" >&3
			return 0
		fi
		if [ "$attempt" -lt "$max_attempts" ]; then
			echo "DPU SCP failed (exit ${status}), retrying in ${retry_interval}s (attempt ${attempt}/${max_attempts})..." >&3
			sleep "$retry_interval"
		fi
	done
	echo "ERROR: DPU SCP failed after ${max_attempts} attempts: $*" >&3
	return "$status"
}

dpu_ssh_ensure_remote_dir() {
	echo "DPU SSH: ensuring remote directory exists: $1" >&2
	dpu_ssh_root mkdir -p "$1"
	echo "DPU SSH: remote directory ready: $1" >&2
}

dpu_ssh_check() {
	echo "DPU SSH: checking connectivity to ${DPU_SSH_USER}@${DPU_SSH_HOST}..." >&2
	_dpu_ssh_require_key
	if ssh "${DPU_SSH_OPTS[@]}" -o ConnectTimeout=2 "${DPU_SSH_USER}@${DPU_SSH_HOST}" "exit" &>/dev/null; then
		echo "DPU SSH: connectivity check passed" >&2
		return 0
	fi
	echo "DPU SSH: connectivity check failed" >&2
	return 1
}

# ──────────────────────────────────────────────────────────────────────────────

cleanup() {
	systemctl stop rshim || true
	systemctl disable rshim || true

	if [ "$CUR_STEP" -eq "$FINAL_STEP" ]; then
		echo "DPU install steps completed successfully"
	else
		echo "DPU install incomplete. Failed at step $CUR_STEP of $FINAL_STEP: ${STEPS[$CUR_STEP]}"
	fi
}

update_progress() {
	CUR_STEP=$1
	local status_string="DPU install: Step $1/$FINAL_STEP: ${STEPS[$1]}"
	printf "\n %s\n" "$status_string"
}

copy_files() {
	update_progress 1

	if [ ! -d "./dpucfg" ]; then
		mkdir ./dpucfg
	fi

	# Decompress only the BFB (bfb-install requires uncompressed) and the
	# configs zip. doca_hbn.tar.gz is intentionally left compressed here —
	# it is transferred to the DPU as-is and decompressed there to avoid
	# pushing several GB over the slow tmfifo interface uncompressed.
	for f in bf-bundle*.bfb.gz doca_container_configs.zip.gz; do
		[ -f "$f" ] && gunzip -f "$f"
	done

	local -a _bfbs=(bf-bundle*.bfb)
	if [ ! -f "${_bfbs[0]}" ]; then
		echo "BFB file not found." >&3
		return 1
	fi
	if [ "${#_bfbs[@]}" -gt 1 ]; then
		echo "ERROR: multiple BFB files staged: ${_bfbs[*]}" >&3
		return 1
	fi
	bfb="${_bfbs[0]}"

	if [ ! -f "doca_hbn.tar.gz" ]; then
		echo "doca_hbn.tar.gz not found."
		return 1
	fi

	if [ ! -f "doca_container_configs.zip" ]; then
		echo "doca_container_configs.zip not found." >&3
		return 1
	fi
	cp doca_container_configs.zip ./dpucfg
	if [ ! -f "doca_hbn_versions.cfg" ]; then
		echo "doca_hbn_versions.cfg not found"
		return 1
	fi
	cp "doca_hbn_versions.cfg" ./dpucfg
}

start_rshim() {
	update_progress 3

	sed -i -e 's/#FORCE_MODE/FORCE_MODE/' /etc/rshim.conf
	systemctl daemon-reload
	systemctl enable rshim
	systemctl start rshim

	echo -n "Waiting for rshim"
	rshim_ready=0
	for i in $(seq 1 12); do
		sleep 5
		if [ -c /dev/rshim0/misc ]; then
			rshim_ready=1
			break
		fi
		echo -n " ."
	done
	echo

	if [ "$rshim_ready" -eq 0 ]; then
		echo "rshim not ready after 60 seconds."
		return 1
	fi

	echo "DISPLAY_LEVEL 2" > /dev/rshim0/misc
	cat /dev/rshim0/misc
}

stage_hbn_config() {
	update_progress 4

	if [ -f "$TOUCHFILE_HBN_CONFIG_STAGED" ]; then
		echo "HBN config staging already completed, skipping."
		return 0
	fi

	if [ ! -f "$STARTUP_YAML" ]; then
		echo "ERROR: startup.yaml not found: $STARTUP_YAML" >&3
		return 1
	fi

	echo "Staging startup.yaml from $STARTUP_YAML"
	cp "$STARTUP_YAML" "./dpucfg/startup.yaml"
	touch "$TOUCHFILE_HBN_CONFIG_STAGED"
}

install_bfb() {
	if [ -f "$TOUCHFILE_BFB_UPDATED" ]; then
		echo "BFB already updated, skipping. Remove $TOUCHFILE_BFB_UPDATED to force another update"
		return 0
	fi

	update_progress 5

	echo "Installing $bfb, will take about 10 to 15 minutes"
	BFB_CFG_FILE="${DPU_BFB_CFG:-./bf.cfg}"
	if [ ! -f "$BFB_CFG_FILE" ]; then
		bfb-install --bfb "$bfb" --rshim rshim0 --verbose || { echo "BFB install failed." >&3; return 1; }
	else
		bfb-install --bfb "$bfb" --config "$BFB_CFG_FILE" --rshim rshim0 --verbose || { echo "BFB install failed." >&3; return 1; }
	fi

	echo "BFB install completed."

	sleep 5

	echo "Waiting for DPU to get ready (timeout: 30 minutes, checked every 60s). Typically takes 5 to 7 minutes..."
	_dpu_ready_timeout=30
	_dpu_ready_attempt=0
	while true; do
		if grep -q "DPU is ready" /dev/rshim0/misc; then
			echo "BFB push complete, DPU is ready (attempt ${_dpu_ready_attempt})."
			break
		fi
		_dpu_ready_attempt=$(( _dpu_ready_attempt + 1 ))
		_dpu_ready_remaining=$(( _dpu_ready_timeout - _dpu_ready_attempt ))
		if [ "$_dpu_ready_attempt" -ge "$_dpu_ready_timeout" ]; then
			echo "ERROR: DPU did not become ready after ${_dpu_ready_timeout} minutes." >&3
			return 1
		fi
		echo "  Attempt ${_dpu_ready_attempt}/${_dpu_ready_timeout}: DPU not ready yet, ${_dpu_ready_remaining} minute(s) remaining..."
		sleep 60
	done

	touch "$TOUCHFILE_BFB_UPDATED"
	sleep 5

}

setup_tmfifo() {
	update_progress 6
	if ! ip addr show dev tmfifo_net0 | grep -q "192.168.100.1"; then
		ip addr add 192.168.100.1/26 dev tmfifo_net0
	else
		echo "IP 192.168.100.1 already assigned to tmfifo_net0"
	fi

	local max_attempts=10
	local attempt=1
	while [ "$attempt" -le "$max_attempts" ]; do
		if ping -c 1 -W 2 192.168.100.2 &>/dev/null; then
			echo "DPU reachable at 192.168.100.2 (attempt ${attempt}/${max_attempts})."
			return 0
		fi
		echo "  Attempt ${attempt}/${max_attempts}: 192.168.100.2 not yet reachable, retrying in 5s..."
		sleep 5
		attempt=$((attempt + 1))
	done
	echo "ERROR: 192.168.100.2 not reachable after ${max_attempts} attempts." >&3
	return 1
}

setup_hbn() {
	update_progress 7

	local doca_hbn_path

	if [ -f "$TOUCHFILE_HBN_SETUP_A" ]; then
		echo "Phase A already complete (touchfile exists), skipping to Phase B..."
		update_progress 8
	else
		if [ ! -d "./dpucfg" ]; then
			echo "ERROR: dpucfg directory not found in $(pwd)" >&3
			return 1
		fi

		if [ ! -f "./dpucfg/startup.yaml" ]; then
			echo "ERROR: ./dpucfg/startup.yaml not found — stage_hbn_config did not produce it." >&3
			return 1
		fi

		echo "Step 7.2: Ensuring remote directory $DPU_REMOTE_DIR exists on DPU..."
		dpu_ssh_ensure_remote_dir "$DPU_REMOTE_DIR"

		echo "Waiting 20s for DPU to settle before file transfer..."
		sleep 20

		# SCP needs brackets around IPv6 hosts to separate the remote path.
		# SSH continues to use the original host.
		local scp_host="$DPU_SSH_HOST"
		if [[ "$scp_host" == *:* && "$scp_host" != \[*\] ]]; then
			scp_host="[$scp_host]"
		fi

		echo "Step 7.3: Copying dpucfg to DPU (startup config, HBN configs zip)..."
		dpu_scp -r dpucfg "root@${scp_host}:/root/" || return 1
		echo "dpucfg transfer complete."

		echo "Step 7.3b: Transferring HBN container image to DPU (compressed — this may take several minutes)..." >&3
		echo "WARNING: This is a large file transferred over the slow tmfifo link (~1-5 Mbps)." >&3
		echo "         SCP may stall on the first attempt. It will be retried up to 3 times automatically." >&3
		echo "         If all retries fail, re-run provision-dpu.sh to resume from this step." >&3
		sleep 10
		dpu_scp doca_hbn.tar.gz "root@${scp_host}:${DPU_REMOTE_DIR}/" || return 1
		echo "HBN image transfer complete."

		echo "Step 7.3c: Decompressing HBN container image on DPU..."
		dpu_ssh_root "gunzip -f '${DPU_REMOTE_DIR}/doca_hbn.tar.gz'"
		echo "HBN image decompressed."

		echo "Step 7.4: Verifying HBN image tar exists in $DPU_REMOTE_DIR..."
		doca_hbn_path=$(dpu_ssh "ls '$DPU_REMOTE_DIR'/doca_hbn*.tar 2>/dev/null | head -1" || true)
		if [ -z "$doca_hbn_path" ]; then
			echo "ERROR: no doca_hbn*.tar found in $DPU_REMOTE_DIR — was it included in the ISO?" >&3
			return 1
		fi
		echo "HBN image: $doca_hbn_path"

		echo "Step 7.5: Verifying startup.yaml exists in $DPU_REMOTE_DIR..."
		if ! dpu_ssh "test -f '$DPU_REMOTE_DIR/startup.yaml'"; then
			echo "ERROR: startup.yaml not found in $DPU_REMOTE_DIR" >&3
			return 1
		fi
		echo "Pre-flight checks passed."

		update_progress 8

		echo "Checking if hbn-dpu-setup has already run (/var/lib/hbn)..."
		if ! dpu_ssh "test -d /var/lib/hbn"; then
			echo "Step 8.1: Extracting doca_container_configs.zip on DPU..."
			dpu_ssh "cd '$DPU_REMOTE_DIR' && unzip -o doca_container_configs.zip"
			echo "Extraction complete."

			echo "Step 8.2: Verifying doca_hbn.yaml exists at $DPU_REMOTE_DIR/$HBN_CONFIG_SRC_DIR/..."
			if ! dpu_ssh "test -f '$DPU_REMOTE_DIR/$HBN_CONFIG_SRC_DIR/doca_hbn.yaml'"; then
				echo "ERROR: doca_hbn.yaml not found at $DPU_REMOTE_DIR/$HBN_CONFIG_SRC_DIR/doca_hbn.yaml" >&3
				echo "  Check that HBN_CONFIG_SRC_DIR in doca_hbn_versions.cfg matches the zip layout." >&3
				return 1
			fi
			echo "doca_hbn.yaml found."

			echo "Step 8.3: Copying doca_hbn.yaml to $DPU_REMOTE_DIR..."
			dpu_ssh "cp '$DPU_REMOTE_DIR/$HBN_CONFIG_SRC_DIR/doca_hbn.yaml' '$DPU_REMOTE_DIR/'"
			echo "doca_hbn.yaml in place."

			echo "Step 8.4: Verifying hbn-dpu-setup.sh exists at $DPU_REMOTE_DIR/$HBN_SCRIPT_DIR/..."
			if ! dpu_ssh "test -f '$DPU_REMOTE_DIR/$HBN_SCRIPT_DIR/hbn-dpu-setup.sh'"; then
				echo "ERROR: hbn-dpu-setup.sh not found at $DPU_REMOTE_DIR/$HBN_SCRIPT_DIR/hbn-dpu-setup.sh" >&3
				echo "  Check that HBN_SCRIPT_DIR in doca_hbn_versions.cfg matches the zip layout." >&3
				return 1
			fi
			echo "hbn-dpu-setup.sh found."

			echo "Step 8.5: Making hbn-dpu-setup.sh executable..."
			dpu_ssh "sudo chmod +x '$DPU_REMOTE_DIR/$HBN_SCRIPT_DIR/hbn-dpu-setup.sh'"

			echo "Step 8.6: Running hbn-dpu-setup.sh (this may take several minutes)..."
			dpu_ssh "sudo '$DPU_REMOTE_DIR/$HBN_SCRIPT_DIR/hbn-dpu-setup.sh'"
			echo "hbn-dpu-setup.sh completed."

			echo "Step 8.7: Syncing filesystem on DPU..."
			dpu_ssh "sudo sync" || true

			echo "Step 8.8: Rebooting DPU..."
			dpu_ssh "sudo reboot" || true

			echo "Waiting 15s for DPU reboot to initiate..."
			sleep 15

			echo "Waiting for DPU SSH to go offline..."
			local _off_max=30 _off_attempt=1
			while [ "$_off_attempt" -le "$_off_max" ]; do
				if ! dpu_ssh_check 2>/dev/null; then
					echo "DPU is offline (attempt ${_off_attempt}/${_off_max})."
					break
				fi
				if [ "$_off_attempt" -eq "$_off_max" ]; then
					echo "WARNING: DPU SSH still reachable after ${_off_max} attempts — proceeding anyway." >&3
					break
				fi
				sleep 5
				_off_attempt=$((_off_attempt + 1))
			done
		else
			echo "hbn-dpu-setup already done (/var/lib/hbn exists), skipping Phase A."
		fi

		echo "Waiting for DPU to come back online after reboot. Typically takes 5 to 7 minutes..."
		local max_attempts=60
		local attempt=1
		while [ "$attempt" -le "$max_attempts" ]; do
			if dpu_ssh_check; then
				echo "DPU is back online (attempt ${attempt}/${max_attempts})."
				break
			fi
			if [ "$attempt" -eq "$max_attempts" ]; then
				echo "ERROR: DPU did not become reachable after ${max_attempts} attempts (${max_attempts}x10s)" >&3
				return 1
			fi
			echo "  Attempt ${attempt}/${max_attempts}: DPU not yet reachable, retrying in 30s..."
			sleep 30
			attempt=$((attempt + 1))
		done

		touch "$TOUCHFILE_HBN_SETUP_A"
		echo "Phase A complete — DPU is online and ready for HBN deployment."
	fi

	doca_hbn_path=$(dpu_ssh_root "ls '$DPU_REMOTE_DIR'/doca_hbn*.tar 2>/dev/null | head -1")
	if [ -z "$doca_hbn_path" ]; then
		echo "ERROR: no doca_hbn*.tar found in $DPU_REMOTE_DIR" >&3
		return 1
	fi

	echo "Checking if HBN container is already deployed (/etc/kubelet.d/doca_hbn.yaml)..."
	if ! dpu_ssh "test -f /etc/kubelet.d/doca_hbn.yaml"; then
		echo "Using HBN image: $doca_hbn_path"

		echo "Step 8.8: Installing startup.yaml to /var/lib/hbn/etc/nvue.d/..."
		dpu_ssh "sudo cp '$DPU_REMOTE_DIR/startup.yaml' /var/lib/hbn/etc/nvue.d/startup.yaml"
		echo "startup.yaml installed."

		echo "Step 8.9: Importing HBN container image (this may take several minutes)..."
		dpu_ssh "sudo ctr --namespace k8s.io image import '$doca_hbn_path'"
		echo "HBN image import complete."

		echo "Step 8.10: Installing kubelet manifest to /etc/kubelet.d/..."
		dpu_ssh "sudo cp '$DPU_REMOTE_DIR/doca_hbn.yaml' /etc/kubelet.d/"
		echo "Kubelet manifest installed."

		echo "Waiting 60s for kubelet to pick up the HBN manifest..."
		sleep 60
		echo "HBN container deployed."
	else
		echo "HBN container already deployed (/etc/kubelet.d/doca_hbn.yaml exists), skipping Phase B."
	fi

	echo "HBN deployment complete."
	touch "$TOUCHFILE_HBN_DEPLOYED"
}

try_setup_netplan() {
	update_progress 9

	if [ -f "$TOUCHFILE_NETPLAN_CONFIGURED" ]; then
		echo "Netplan already configured (touchfile exists), skipping."
		return 0
	fi

	if [ -z "${SERVER_NAME:-}" ]; then
		echo "SERVER_NAME not set — skipping pre-power-cycle netplan setup."
		return 0
	fi

	echo "Waiting 30s before configuring netplan (BlueField interface may need time to appear)..." >&3
	sleep 30

	echo "Attempting to configure netplan before power cycle..." >&3
	if bash "$SCRIPTS_DIR/setup_netplan.sh" --server-name "$SERVER_NAME"; then
		touch "$TOUCHFILE_NETPLAN_CONFIGURED"
		echo "Netplan configured successfully before power cycle."
		echo "Network will come up automatically after power cycle."
	else
		echo "Netplan setup failed (BlueField interface may not be detectable yet)."
		echo "Will retry in post-power-cycle.sh."
	fi
}

power_cycle() {
	update_progress 10

	echo "" >&3
	echo "A power cycle is required to complete DPU firmware installation." >&3
	echo "NOTE: A simple reboot is not sufficient — the host must be fully power cycled." >&3
	echo "" >&3

	if ! command -v ipmitool &>/dev/null; then
		echo "WARNING: ipmitool not found — cannot trigger power cycle automatically." >&3
		echo "Please power cycle the host manually via the BMC (unmount any virtual media first)." >&3
		echo "Once the host is back up, run post-power-cycle.sh to complete provisioning." >&3
		return 1
	fi

	read -r -p "Press Enter to trigger power cycle via ipmitool, or Ctrl+C to abort and power cycle manually: "

	sync

	if ! sudo ipmitool chassis power cycle; then
		echo "ipmitool power cycle failed." >&3
		echo "Please power cycle the host manually via the BMC." >&3
		echo "Once the host is back up, run post-power-cycle.sh to complete provisioning." >&3
		return 1
	fi
}

check_hbn_container() {
	start_rshim
	setup_tmfifo

	sleep 10

	dpu_ssh_prepare

	echo "Step 10.1: Waiting for doca-hbn container to start on DPU..."
	local max_attempts=10
	local attempt=1
	local container_id=""
	while [ "$attempt" -le "$max_attempts" ]; do
		container_id=$(dpu_ssh "sudo crictl ps --name doca-hbn -q 2>/dev/null" || true)
		if [ -n "$container_id" ]; then
			echo "doca-hbn container found: $container_id"
			break
		fi
		if [ "$attempt" -eq "$max_attempts" ]; then
			echo "ERROR: doca-hbn container did not appear after ${max_attempts} attempts" >&3
			return 1
		fi
		echo "  Attempt ${attempt}/${max_attempts}: container not yet started, retrying in 5s..."
		sleep 5
		attempt=$((attempt + 1))
	done

	echo "Step 10.2: Waiting for doca-hbn container $container_id to reach running state..."
	local _has_jq
	_has_jq=$(dpu_ssh "command -v jq >/dev/null 2>&1 && echo 1 || echo 0" 2>/dev/null || echo 0)
	attempt=1
	local state=""
	while [ "$attempt" -le "$max_attempts" ]; do
		if [ "$_has_jq" = "1" ]; then
			state=$(dpu_ssh "sudo crictl inspect -o json '${container_id}' | jq -r '.status.state'" || true)
		else
			state=$(dpu_ssh "sudo crictl ps --name doca-hbn --state Running -q | grep -q . && echo CONTAINER_RUNNING || true" || true)
		fi
		if [ "$state" = "CONTAINER_RUNNING" ]; then
			echo "Container $container_id is running."
			break
		fi
		if [ "$attempt" -eq "$max_attempts" ]; then
			echo "ERROR: doca-hbn container did not reach running state after ${max_attempts} attempts (last state: ${state})" >&3
			return 1
		fi
		echo "  Attempt ${attempt}/${max_attempts}: state=${state}, retrying in 5s..."
		sleep 5
		attempt=$((attempt + 1))
	done

	echo "HBN is ready"
}

# ── Run only when executed directly (not sourced) ─────────────────────────────

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then

	usage() {
		echo "Usage: $0 --startup-yaml <path> --server-name <name>"
		echo ""
		echo "Options:"
		echo "  --startup-yaml <path>   Path to the HBN startup.yaml for this node (required)"
		echo "  --server-name <name>    Hostname of this server (required)"
		echo "  --help                  Show this help message"
		exit 1
	}

	STARTUP_YAML=""
	SERVER_NAME=""

	while [[ $# -gt 0 ]]; do
		case "$1" in
			--startup-yaml)
				STARTUP_YAML="${2:-}"
				if [ -z "$STARTUP_YAML" ]; then
					echo "ERROR: --startup-yaml requires a path argument" >&3
					usage
				fi
				shift 2
				;;
			--server-name)
				SERVER_NAME="${2:-}"
				if [ -z "$SERVER_NAME" ]; then
					echo "ERROR: --server-name requires an argument" >&3
					usage
				fi
				shift 2
				;;
			--help)
				usage
				;;
			*)
				echo "ERROR: unknown option: $1" >&3
				usage
				;;
		esac
	done

	[ -z "$STARTUP_YAML" ] && { echo "ERROR: --startup-yaml is required" >&3; usage; }
	[ -z "$SERVER_NAME" ]  && { echo "ERROR: --server-name is required" >&3; usage; }
	[ -f "$STARTUP_YAML" ] || { echo "ERROR: startup.yaml not found: $STARTUP_YAML" >&3; exit 1; }

	set -eux
	trap cleanup EXIT
	cd "$SCRIPTS_DIR"
	mkdir -p "$TOUCHFILES_DIR"

	copy_files
	dpu_ssh_prepare
	start_rshim
	stage_hbn_config
	install_bfb
	setup_tmfifo
	setup_hbn
	try_setup_netplan
	power_cycle
fi
