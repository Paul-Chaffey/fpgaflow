#!/usr/bin/env bash
# Build openFPGALoader v1.2.0 with fpgaflow's part-table patch (XC3S700AN,
# IDCODE 0x02628093 from ISE 14.7's BSDL) into ~/tools/openfpgaloader-fpgaflow.
#
# Only the Xilinx Platform Cable USB driver and Xilinx FPGA support are built,
# so the only build dependency beyond a C++ compiler and CMake is libusb-1.0.
# Enable more cables/vendors by dropping the -DENABLE_*_ALL=OFF switches
# (they need libftdi1 and hidapi development packages).
#
# Submitted upstream: https://github.com/trabucayre/openFPGALoader/pull/771
# Once a release includes it, the stock openFPGALoader works and this
# script is no longer needed.
set -euo pipefail

repo_dir="$(cd "$(dirname "$0")/.." && pwd)"
src="${HOME}/tools/openFPGALoader-src"
prefix="${HOME}/tools/openfpgaloader-fpgaflow"

if [ ! -d "${src}/.git" ]; then
    git clone --depth 1 --branch v1.2.0 https://github.com/trabucayre/openFPGALoader.git "${src}"
fi
cd "${src}"
if git apply --check "${repo_dir}/third_party/openfpgaloader/0001-add-xc3s700an.patch" 2>/dev/null; then
    git apply "${repo_dir}/third_party/openfpgaloader/0001-add-xc3s700an.patch"
fi

cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="${prefix}" \
    -DENABLE_CABLE_ALL=OFF -DENABLE_XILINX_PLATFORM_CABLE_USB=ON \
    -DENABLE_VENDORS_ALL=OFF -DENABLE_XILINX_SUPPORT=ON
cmake --build build -j"$(nproc)"
cmake --install build
echo "installed: ${prefix}/bin/openFPGALoader"
