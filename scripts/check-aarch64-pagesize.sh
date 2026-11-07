#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: check-aarch64-pagesize.sh [--allow-empty] [--min-page-size BYTES] [ROOT]

Scans ROOT for aarch64 ELF executables/shared objects and verifies every
PT_LOAD segment can be mapped on a kernel with the requested page size.

Options:
  --allow-empty           Succeed when no aarch64 ELF files are found.
  --min-page-size BYTES   Minimum supported kernel page size. Default: 65536.
  -h, --help              Show this help.
EOF
}

root="."
min_page_size=$((64 * 1024))
allow_empty=false

while (($#)); do
    case "$1" in
        --allow-empty)
            allow_empty=true
            shift
            ;;
        --min-page-size)
            if (($# < 2)); then
                echo "error: --min-page-size requires a value" >&2
                exit 2
            fi
            min_page_size="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        --)
            shift
            break
            ;;
        -*)
            echo "error: unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
        *)
            root="$1"
            shift
            if (($#)); then
                echo "error: only one ROOT argument is supported" >&2
                usage >&2
                exit 2
            fi
            ;;
    esac
done

if (($#)); then
    root="$1"
    shift
fi

if (($#)); then
    echo "error: only one ROOT argument is supported" >&2
    usage >&2
    exit 2
fi

if ! [[ "$min_page_size" =~ ^[0-9]+$ ]] || (( min_page_size <= 0 )); then
    echo "error: --min-page-size must be a positive integer byte count" >&2
    exit 2
fi

if [[ ! -e "$root" ]]; then
    if [[ "$allow_empty" == true ]]; then
        printf 'SKIP missing %s\n' "$root"
        printf 'Checked 0 aarch64 ELF executable/shared object(s)\n'
        exit 0
    fi

    echo "error: path does not exist: $root" >&2
    exit 2
fi

checked=0
failed=0

if ! command -v readelf >/dev/null 2>&1; then
    echo "error: required command not found: readelf" >&2
    exit 2
fi

while IFS= read -r -d '' candidate; do
    elf_header="$(readelf -h "$candidate" 2>/dev/null || true)"

    # Keep only aarch64 ELF files.
    machine="$(awk -F: '/Machine:/ { gsub(/^[[:space:]]+/, "", $2); print $2; exit }' <<<"$elf_header")"
    if [[ "$machine" != "AArch64" ]]; then
        continue
    fi

    # Ignore relocatable object files; they do not have meaningful LOAD alignment.
    elf_type="$(awk '/Type:/ { print $2; exit }' <<<"$elf_header")"
    if [[ "$elf_type" != "EXEC" && "$elf_type" != "DYN" ]]; then
        continue
    fi

    segment_count=0
    file_failed=0
    while read -r offset vaddr align; do
        if [[ -z "$offset" || -z "$vaddr" || -z "$align" ]]; then
            continue
        fi

        segment_count=$((segment_count + 1))
        align_value=$((align))
        offset_value=$((offset))
        vaddr_value=$((vaddr))

        if (( align_value < min_page_size )); then
            printf 'FAIL p_align=0x%x below required 0x%x %s\n' \
                "$align_value" "$min_page_size" "$candidate"
            file_failed=1
        fi

        if (( (offset_value % min_page_size) != (vaddr_value % min_page_size) )); then
            printf 'FAIL p_offset=0x%x p_vaddr=0x%x not congruent modulo 0x%x %s\n' \
                "$offset_value" "$vaddr_value" "$min_page_size" "$candidate"
            file_failed=1
        fi
    done < <(readelf -lW "$candidate" 2>/dev/null | awk '$1 == "LOAD" { print $2, $3, $NF }')

    checked=$((checked + 1))

    if (( segment_count == 0 )); then
        printf 'FAIL no PT_LOAD segments found %s\n' "$candidate"
        failed=1
    elif (( file_failed )); then
        failed=1
    else
        printf 'OK   page_size=0x%x load_segments=%d %s\n' \
            "$min_page_size" "$segment_count" "$candidate"
    fi
done < <(find "$root" -type f -print0)

printf 'Checked %d aarch64 ELF executable/shared object(s)\n' "$checked"
if (( checked == 0 )) && [[ "$allow_empty" != true ]]; then
    echo "FAIL no aarch64 ELF executable/shared object(s) found" >&2
    failed=1
fi

exit "$failed"
