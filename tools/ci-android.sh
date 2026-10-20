#!/bin/sh
# Runs inside the pinned Rust supplier container with /src read-only and /out writable.
set -eu
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_DEBUG=0
mkdir -p /out/acquire /out/unzip
cd /out/acquire
apt-get -o APT::Sandbox::User=root update -qq
apt-get -o APT::Sandbox::User=root download unzip
dpkg-deb --extract ./unzip_*.deb /out/unzip
export PATH=/out/unzip/usr/bin:$PATH
cd /src
cargo fetch --locked
cargo build -p skvoz-core-tools --release --locked --offline
CORE_TOOLS=/out/target/release/skvoz-core-tools
export CORE_NDK_ARCHIVE=/out/acquire/android-ndk-r30-linux.zip
curl --fail --location --proto '=https' --connect-timeout 15 --max-time 300 \
    --max-filesize 738633529 --output "$CORE_NDK_ARCHIVE" \
    https://dl.google.com/android/repository/android-ndk-r30-linux.zip
# prepare-android verifies the pinned archive size and SHA1 before selective extraction.
ln -s "$(rustc --print sysroot)" /toolchain
export CORE_SHARED_TOOLS=/out/android-tools
timeout --signal=TERM --kill-after=10s 900s \
    "$CORE_TOOLS" prepare-android "$CORE_NDK_ARCHIVE" "$CORE_SHARED_TOOLS"
export SKVOZ_CORE_ANDROID_NDK="$CORE_SHARED_TOOLS/sdk/ndk/30.0.16248370"
export SKVOZ_CORE_ANDROID_SYSROOT="$CORE_SHARED_TOOLS/rust-android"
export SKVOZ_CORE_ANDROID_NDK_ARCHIVE="$CORE_NDK_ARCHIVE"
export RUSTFLAGS="-C link-arg=-Wl,-z,max-page-size=16384 --sysroot $SKVOZ_CORE_ANDROID_SYSROOT"
CORE_NDK_BIN="$SKVOZ_CORE_ANDROID_NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$CORE_NDK_BIN/aarch64-linux-android31-clang"
export CC_aarch64_linux_android="$CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"
export AR_aarch64_linux_android="$CORE_NDK_BIN/llvm-ar"
export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$CORE_NDK_BIN/x86_64-linux-android31-clang"
export CC_x86_64_linux_android="$CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER"
export AR_x86_64_linux_android="$CORE_NDK_BIN/llvm-ar"
for target in aarch64-linux-android x86_64-linux-android; do
    export SKVOZ_CORE_ANDROID_STD_ARCHIVE="$CORE_SHARED_TOOLS/downloads/rust-std-1.92.0-$target.tar.xz"
    timeout --signal=TERM --kill-after=10s 900s \
        cargo build -p skvoz-core-abi --target "$target" --release --locked --offline
    "$CORE_TOOLS" package-android "$target" "/out/target/$target/release/libskvoz_core.so" "/out/artifact-$target"
    "$CORE_TOOLS" archive "/out/artifact-$target" "$SKVOZ_CORE_SOURCE_REVISION" /out/release
done
