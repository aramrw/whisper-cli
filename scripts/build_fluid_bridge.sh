#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
STAGE_DIR="${REPO_ROOT}/target/fluid_bridge"
SOURCES_DIR="${STAGE_DIR}/Sources/ParakeetBridge"
OUT_LIB="${REPO_ROOT}/lib/libParakeetBridge.dylib"
FLUIDAUDIO_REV="e2ea498dafd6db9b3f18f0f644bc35312b3a7679"

rm -rf "${SOURCES_DIR}"
mkdir -p "${SOURCES_DIR}" "${REPO_ROOT}/lib"

cat > "${STAGE_DIR}/Package.swift" <<EOF
// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "ParakeetBridge",
    platforms: [
        .macOS(.v14),
    ],
    products: [
        .library(
            name: "ParakeetBridge",
            type: .dynamic,
            targets: ["ParakeetBridge"]
        ),
    ],
    dependencies: [
        .package(
            url: "https://github.com/FluidInference/FluidAudio.git",
            revision: "${FLUIDAUDIO_REV}",
            traits: []
        ),
    ],
    targets: [
        .target(
            name: "ParakeetBridge",
            dependencies: [
                .product(name: "FluidAudio", package: "FluidAudio"),
            ],
            swiftSettings: [
                .swiftLanguageMode(.v5),
            ]
        ),
    ]
)
EOF

cp "${REPO_ROOT}/src/ParakeetBridge.swift" "${SOURCES_DIR}/ParakeetBridge.swift"
cp "${REPO_ROOT}/src/ParakeetUltraBridge.swift" "${SOURCES_DIR}/ParakeetUltraBridge.swift"

echo "Building libParakeetBridge.dylib against FluidAudio (${FLUIDAUDIO_REV})..."
swift build -c release --package-path "${STAGE_DIR}"

BUILT_DYLIB="${STAGE_DIR}/.build/release/libParakeetBridge.dylib"
if [[ ! -f "${BUILT_DYLIB}" ]]; then
    echo "Error: expected dylib not found at ${BUILT_DYLIB}" >&2
    exit 1
fi

TMP_DYLIB="${OUT_LIB}.tmp.$$"
cp "${BUILT_DYLIB}" "${TMP_DYLIB}"
chmod 755 "${TMP_DYLIB}"
install_name_tool -id @rpath/libParakeetBridge.dylib "${TMP_DYLIB}"

EXPECTED_SYMBOLS=(
    wisp_parakeet_available
    wisp_parakeet_start
    wisp_parakeet_is_busy
    wisp_parakeet_decode
    wisp_parakeet_stop
    wisp_parakeet_ultra_available
    wisp_parakeet_ultra_start
    wisp_parakeet_ultra_is_busy
    wisp_parakeet_ultra_decode
    wisp_parakeet_ultra_stop
    wisp_parakeet_ultra_transcribe_file
    wisp_parakeet_ultra_free_string
)

EXPORTED="$(nm -gU "${TMP_DYLIB}")"
for sym in "${EXPECTED_SYMBOLS[@]}"; do
    if ! grep -q "_${sym}$" <<<"${EXPORTED}"; then
        echo "Error: missing exported symbol ${sym} in built dylib" >&2
        rm -f "${TMP_DYLIB}"
        exit 1
    fi
done

mv "${TMP_DYLIB}" "${OUT_LIB}"
echo "Verified and wrote ${OUT_LIB} ($(du -h "${OUT_LIB}" | awk '{print $1}'))"
