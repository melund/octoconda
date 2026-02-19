#!/bin/sh

set -x

WORK_DIR="${PWD}"

SRC="${PKG_NAME}-${PKG_VERSION}-${target_platform}"
SRC_FILE="${WORK_DIR}/${SRC}"

if ! test -f "$SRC_FILE"; then
    echo "${SRC} not found"
    echo "Work directory contents is:"
    ls -alF "${WORK_DIR}"
    exit 1
fi

# Iteratively decompress and extract using file(1) to detect type.
# This handles multi-layer formats (e.g. gzip-compressed tar) by looping:
# each iteration peels off one layer of compression until we reach an
# archive or bare binary.
MAX_STEPS=5
STEP=0
while [ "$STEP" -lt "$MAX_STEPS" ]; do
    STEP=$((STEP + 1))
    FILE_TYPE=$(file -b "$SRC_FILE")
    echo "Step ${STEP}: ${FILE_TYPE}"
    case "$FILE_TYPE" in
        *Zip\ archive* | *zip\ archive*)
            ( cd "$PREFIX" && unzip -n "$SRC_FILE" )
            # Delete extra stuff Macs apparently stuffed into zip files:
            rm -rf "$PREFIX/__MACOSX" || true
            break
            ;;
        *tar\ archive*)
            ( cd "$PREFIX" && tar -xf "$SRC_FILE" )
            break
            ;;
        *gzip\ compressed*)
            gzip -dc < "$SRC_FILE" > "${SRC_FILE}.tmp"
            mv "${SRC_FILE}.tmp" "$SRC_FILE"
            ;;
        *XZ\ compressed*)
            xz -dc < "$SRC_FILE" > "${SRC_FILE}.tmp"
            mv "${SRC_FILE}.tmp" "$SRC_FILE"
            ;;
        *Zstandard\ compressed* | *zstd\ compressed*)
            zstd -dc < "$SRC_FILE" > "${SRC_FILE}.tmp"
            mv "${SRC_FILE}.tmp" "$SRC_FILE"
            ;;
        *bzip2\ compressed*)
            bzip2 -dc < "$SRC_FILE" > "${SRC_FILE}.tmp"
            mv "${SRC_FILE}.tmp" "$SRC_FILE"
            ;;
        *PE32* | *PE32+*)
            mkdir -p "${PREFIX}/bin"
            cp "$SRC_FILE" "${PREFIX}/bin/${PKG_NAME}.exe"
            chmod 755 "${PREFIX}/bin/${PKG_NAME}.exe"
            break
            ;;
        *)
            # Bare binary or unknown — copy as executable
            cp "$SRC_FILE" "${PREFIX}/${PKG_NAME}"
            chmod 755 "${PREFIX}/${PKG_NAME}"
            break
            ;;
    esac
done

if [ "$STEP" -ge "$MAX_STEPS" ]; then
    echo "Failed to fully extract ${SRC} after ${MAX_STEPS} decompression steps"
    echo "Last file type: $(file -b "$SRC_FILE")"
    exit 1
fi

pushd "$PREFIX" || exit 3

shopt -s dotglob

# Move everything out of a "foo-arch-version" folder
while [ $(find . -mindepth 1 -maxdepth 1 -type d -not -name conda-meta | wc -l) -eq 1 ]; do
    if test -d "bin"; then
        echo "Found only a bin subdir, this looks good"
        break
    else
        # move everything up a level, using a temp name to avoid
        # conflicts when the directory contains a file with the same name
        SUBDIR=$(find . -mindepth 1 -maxdepth 1 -type d -not -name conda-meta)
        TMPNAME=".strip-$$"
        mv "${SUBDIR}" "${TMPNAME}"
        mv "${TMPNAME}"/* . || true
        rmdir "${TMPNAME}"
    fi
done

# Move all executable files into bin
mkdir -p bin
mkdir -p extras

for f in *; do
    if test -f "${f}"; then
        if file "${f}" | grep "executable"; then
            chmod 755 "${f}"
        fi

        if test -x "${f}"; then
            mv "${f}" bin
        else
            case "$f" in
            *.exe|*.bat|*.com)
                mv "${f}" bin
                ;;
            *)
                mv "${f}" extras
                ;;
            esac
        fi
    elif test -d "${f}"; then
        case "${f}" in
        conda-meta|bin|etc|include|lib|man|share|ssl|extras)
            ;;
        *)
            mv "${f}" extras
        esac
    fi
done

cd "${PREFIX}/bin" || exit

for f in *; do
    if [[ "$f" == *"-${PKG_VERSION}"* ]]; then
        short="${f%%-*}"
        mv "${f}" "${short}"
    fi
done

shopt -u dotglob
