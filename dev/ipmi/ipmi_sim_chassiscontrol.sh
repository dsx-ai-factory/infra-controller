#!/bin/sh
#
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
# http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
#

#
# An example script for handling external power control.

# It's parameters are:
#
#  ipmi_sim_chassiscontrol <device> get [parm [parm ...]]
#  ipmi_sim_chassiscontrol <device> set [parm val [parm val ...]]
#
# where <device> is the particular target to reset and parm is either
# "power", "reset", or "boot".
#
# The output of the "get" is "<parm>:<value>" for each listed parm,
# and only power is listed, you cannot fetch reset.
# The output of the "set" is empty on success.  Error output goes to
# standard out (so it can be captured in the simulator) and the program
# returns an error.
#
# The values for power and reset are either "1" or "0".  Note that
# reset does a pulse, it does not set the reset line level.
#
# The value for boot is either "none", "pxe" or "default".

prog=$0
event_fifo=./chassis-control.fifo

device=$1
if [ "x$device" = "x" ]; then
    echo "No device given"
    exit 1;
fi
shift

op=$1
if [ "x$op" = "x" ]; then
    echo "No operation given"
    exit 1
fi
shift

do_get() {
    while [ "x$1" != "x" ]; do
	case $1 in
	    power)
		val=0
		;;

	    boot)
		val=default
		;;

	    # Note that reset has no get

	    *)
		echo "Invalid parameter: $1"
		exit 1
		;;
	esac

	echo "$1:$val"
	shift
    done
}

do_set() {
    while [ "x$1" != "x" ]; do
	parm="$1"
	shift
	if [ "x$1" = "x" ]; then
	    echo "No value present for parameter $parm"
	    exit 1
	fi
	val="$1"
	shift

	case $parm in
	    power)
		;;

	    reset)
		if [ "$val" = "1" ]; then
		    if ! printf '%s\n' chassis-reset > "$event_fifo"; then
			echo "Could not report chassis reset"
			exit 1
		    fi
		fi
		;;

	    boot)
		case $val in
		    none)
			;;
		    pxe)
			;;
	            cdrom)
			;;
		    bios)
		        ;;
		    default)
	               ;;
		    *)
			echo "Invalid boot value: $val"
			exit 1
			;;
		esac
		;;

            identify)
		interval=$val
		force=$1
		shift
		;;

	    *)
		echo "Invalid parameter: $parm"
		exit 1
		;;
	esac
    done
}

do_check() {
    # Check is not supported for chassis control
    exit 1
}

case $op in
    get)
	do_get $@
	;;
    set)
	do_set $@
	;;

    check)
	do_check $@
	;;

*)
	echo "Unknown operation: $op"
	exit 1
esac
