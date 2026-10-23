FROM rust:1.92.0-bookworm@sha256:e90e846de4124376164ddfbaab4b0774c7bdeef5e738866295e5a90a34a307a2 AS base

WORKDIR /src
RUN apt-get update -qq \
    && apt-get install -y --no-install-recommends unzip util-linux \
    && rm -rf /var/lib/apt/lists/* \
    && rustup component add rustfmt clippy \
    && ln -s "$(rustc --print sysroot)" /toolchain \
    && mkdir -p /out \
    && chown nobody:nogroup /out
USER nobody:nogroup
SHELL ["setpriv", "--no-new-privs", "/bin/sh", "-eu", "-c"]
ENV CARGO_HOME=/out/cargo \
    CARGO_TARGET_DIR=/out/target \
    CARGO_INCREMENTAL=0 \
    CORE_TOOLS=/out/target/release/skvoz-core-tools \
    GIT_CONFIG_COUNT=1 \
    GIT_CONFIG_KEY_0=safe.directory \
    GIT_CONFIG_VALUE_0=/src
WORKDIR /src/src

FROM base AS test-tools
USER root
RUN mkdir /tmp/nats \
    && curl --fail --location --output /tmp/nats/server.tar.gz \
       https://github.com/nats-io/nats-server/releases/download/v2.15.1/nats-server-v2.15.1-linux-amd64.tar.gz \
    && echo 'bf5edc7b1ce98e885c368cb43a886eb148dc5d0551d99f3cf6141fb5e1954c70  /tmp/nats/server.tar.gz' | sha256sum -c - \
    && tar -xzf /tmp/nats/server.tar.gz -C /tmp/nats \
    && install -m 0755 /tmp/nats/nats-server-v2.15.1-linux-amd64/nats-server /usr/local/bin/nats-server \
    && test "$(nats-server --version)" = 'nats-server: v2.15.1' \
    && command -v openssl \
    && rm -rf /tmp/nats

USER nobody:nogroup

FROM base AS tools
# Keep fetched dependencies and the host tool in shared layers, outside sources.
RUN --mount=type=bind,target=/src cargo fetch --locked
RUN --mount=type=bind,target=/src \
    cargo build -p skvoz-core-tools --release --locked --offline
ARG SOURCE_REVISION
ENV SKVOZ_CORE_SOURCE_REVISION=$SOURCE_REVISION

FROM tools AS linux
RUN --mount=type=bind,target=/src \
    cargo build -p skvoz-core-abi --release --locked --offline \
    && "$CORE_TOOLS" package /out/target/release/libskvoz_core.so /out/artifacts/x86_64-unknown-linux-gnu
RUN --mount=type=bind,target=/src \
    export SKVOZ_CORE_ARTIFACT=/out/artifacts/x86_64-unknown-linux-gnu; \
    cargo build -p skvoz-daemon --release --locked --offline; \
    mkdir "$SKVOZ_CORE_ARTIFACT/bin"; \
    cp /out/target/release/skvoz-core-daemon "$SKVOZ_CORE_ARTIFACT/bin/"

FROM linux AS tests-linux
COPY --from=test-tools /usr/local/bin/nats-server /usr/local/bin/nats-server
ENV SKVOZ_CORE_ARTIFACT=/out/artifacts/x86_64-unknown-linux-gnu \
    LD_LIBRARY_PATH=/out/artifacts/x86_64-unknown-linux-gnu/lib
RUN --mount=type=bind,target=/src \
    cargo fmt --all -- --check \
    && cargo check -p skvoz-core --no-default-features --lib --locked --offline \
    && cargo test -p skvoz-core --features fault-injection --locked --offline -- --test-threads=1 \
    && cargo test -p skvoz-core-contracts --features runtime,codec --release --locked --offline \
    && cargo test -p skvoz-core-abi --lib --release --locked --offline -- --test-threads=1
RUN --mount=type=bind,target=/src \
    cargo test -p skvoz-core-binding -p skvoz-daemon -p skvoz-tcp --release --locked --offline -- --test-threads=1; \
    script=$(find /out/target/release/build -path '*/skvoz-core-binding-*/build-script-build' -type f | head -n 1); \
    SKVOZ_CORE_BUILD_SCRIPT="$script" cargo test -p skvoz-core-binding --test artifact_contract --release --locked --offline -- --ignored; \
    env -u LD_LIBRARY_PATH "$SKVOZ_CORE_ARTIFACT/bin/skvoz-core-daemon" --version; \
    ldd "$SKVOZ_CORE_ARTIFACT/bin/skvoz-core-daemon"; \
    cargo run -p skvoz-daemon --example daemon-smoke --release --locked --offline -- "$SKVOZ_CORE_ARTIFACT/bin/skvoz-core-daemon"; \
    cargo build -p skvoz-testbench --release --locked --offline
RUN --mount=type=bind,target=/src \
    unset SKVOZ_CORE_SOURCE_REVISION; \
    cargo test -p skvoz-core-tools --bin skvoz-core-tools --test export_contract --release --locked --offline

FROM scratch AS linux-artifacts
COPY --from=tests-linux /out/artifacts/x86_64-unknown-linux-gnu/ /x86_64-unknown-linux-gnu/

FROM tools AS android
RUN --mount=type=bind,target=/src \
    mkdir /out/acquire \
    && curl --fail --location --proto '=https' --connect-timeout 15 --max-time 300 \
       --max-filesize 738633529 --output /out/acquire/android-ndk-r30-linux.zip \
       https://dl.google.com/android/repository/android-ndk-r30-linux.zip \
    && timeout --signal=TERM --kill-after=10s 900s \
       "$CORE_TOOLS" prepare-android /out/acquire/android-ndk-r30-linux.zip /out/android-tools
RUN --mount=type=bind,target=/src \
    export CARGO_PROFILE_RELEASE_DEBUG=0 \
        SKVOZ_CORE_ANDROID_NDK=/out/android-tools/sdk/ndk/30.0.16248370 \
        SKVOZ_CORE_ANDROID_SYSROOT=/out/android-tools/rust-android \
        SKVOZ_CORE_ANDROID_NDK_ARCHIVE=/out/acquire/android-ndk-r30-linux.zip; \
    export RUSTFLAGS="-C link-arg=-Wl,-z,max-page-size=16384 --sysroot $SKVOZ_CORE_ANDROID_SYSROOT"; \
    ndk_bin="$SKVOZ_CORE_ANDROID_NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"; \
    export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$ndk_bin/aarch64-linux-android31-clang" \
        CC_aarch64_linux_android="$ndk_bin/aarch64-linux-android31-clang" \
        AR_aarch64_linux_android="$ndk_bin/llvm-ar" \
        CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$ndk_bin/x86_64-linux-android31-clang" \
        CC_x86_64_linux_android="$ndk_bin/x86_64-linux-android31-clang" \
        AR_x86_64_linux_android="$ndk_bin/llvm-ar"; \
    for target in aarch64-linux-android x86_64-linux-android; do \
        export SKVOZ_CORE_ANDROID_STD_ARCHIVE="/out/android-tools/downloads/rust-std-1.92.0-$target.tar.xz"; \
        timeout --signal=TERM --kill-after=10s 900s \
            cargo build -p skvoz-core-abi --target "$target" --release --locked --offline; \
        "$CORE_TOOLS" package-android "$target" "/out/target/$target/release/libskvoz_core.so" "/out/artifacts/$target"; \
    done

FROM scratch AS android-artifacts
COPY --from=android /out/artifacts/aarch64-linux-android/ /aarch64-linux-android/
COPY --from=android /out/artifacts/x86_64-linux-android/ /x86_64-linux-android/

FROM tests-linux AS tests-release
COPY --from=android /out/artifacts/aarch64-linux-android/ /out/artifacts/aarch64-linux-android/
COPY --from=android /out/artifacts/x86_64-linux-android/ /out/artifacts/x86_64-linux-android/
ENV SKVOZ_CORE_ANDROID_ARTIFACT=/out/artifacts/x86_64-linux-android \
    SKVOZ_CORE_RELEASE_FIXTURES=/out/artifacts
RUN --mount=type=bind,target=/src \
    unset SKVOZ_CORE_SOURCE_REVISION; \
    cargo test -p skvoz-core-tools --test export_contract --release --locked --offline -- --ignored; \
    cargo test -p skvoz-core-tools --test release_contract --release --locked --offline -- --include-ignored

FROM tests-release AS release
RUN --mount=type=bind,target=/src \
    for target in x86_64-unknown-linux-gnu aarch64-linux-android x86_64-linux-android; do \
        "$CORE_TOOLS" archive "/out/artifacts/$target" "$SKVOZ_CORE_SOURCE_REVISION" /out/release; \
    done; \
    "$CORE_TOOLS" checksums /out/release "$SKVOZ_CORE_SOURCE_REVISION"; \
    cp "$CORE_TOOLS" /out/release/

FROM scratch AS artifacts
COPY --from=release /out/release/ /
