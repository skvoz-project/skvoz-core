# Core supplier для Android

Поддержаны два явных профиля упаковки: `aarch64-linux-android` → `arm64-v8a`
и `x86_64-linux-android` → `x86_64`. Профили определены в
[`tools/android-profiles.json`](../tools/android-profiles.json): Rust 1.92.0,
NDK r30 revision 30.0.16248370, minimum API 31, ELF LOAD alignment 16384.
Для обоих профилей проверены supplier cross-compile, ELF dependency closure,
экспорт ABI v1, compile-time layout bounds и16 KiB LOAD alignment. Это scope
готовых supplier outputs; загрузка Android, JNI и APK проверяются потребителем
отдельно. Каждый target получает собственную библиотеку и manifest.

## Подготовленная среда

Поставщик использует Linux x86_64 host toolchain Rust 1.92.0, GNU `readelf`,
`tar` с xz, `unzip`, `zipinfo`, `curl`, `sha1sum`, `sha256sum` и заранее
подготовленные Cargo registry/cache.
Нужны закреплённый NDK archive и оба Rust std archive из файла профилей.
Перед распаковкой проверяются размер и digest каждого archive; NDK имеет
исходный опубликованный SHA1, упаковщик дополнительно записывает фактический SHA256.
Распаковка выполняется в отдельный каталог с проверкой путей и symlinks;
инструменты host и существующие SDK не изменяются.

Достаточный NDK layout под `toolchains/llvm/prebuilt/linux-x86_64/`:

- `bin/clang`, `clang-21`, `clang++`, `ld.lld`, `lld`, `llvm-ar`,
  `llvm-objcopy`, `llvm-strip`, оба `*-linux-android31-clang`;
- `lib/clang/`, `sysroot/usr/include/`;
- верхнеуровневые stubs и `31/` под обоими target-каталогами `sysroot/usr/lib/`.

Сохраните `source.properties`. Приватный Rust sysroot содержит
`lib/rustlib/aarch64-linux-android/lib/` и
`lib/rustlib/x86_64-linux-android/lib/` из соответствующих закреплённых archives,
а host `lib/rustlib/x86_64-unknown-linux-gnu` доступен из host toolchain.
Точность staging относительно проверенных archives входит в подготовку среды;
упаковщик записывает также hashes реально установленных target std files,
clang и llvm-ar. Для Core не нужны Gradle, JDK или Android SDK platform packages.

Следующий пример предназначен для локальной проверки. Подготовка и сборки
выполняются последовательно в ограниченной среде:
до4 CPU,3 GiB RAM, без swap, PID256 и один Cargo job; подготовка и каждый target
имеют отдельный конечный deadline до900 секунд. Проверяйте фактические cgroup
limits и cleanup, а не только параметры запуска. Пример ниже предполагает
стандартный Cargo `target/`, подготовленный registry и абсолютные пути без пробелов.
Host Rust toolchain должен быть доступен read-only как `/toolchain`: на него
указывает host rustlib link подготовленного sysroot. GitHub supplier запускает
`tools/ci-android.sh` в закреплённом Docker image с ресурсами runner, без
локальных cgroup/I/O предпосылок; версии, target-проверки и deadlines сохраняются.
Число Cargo jobs выбирает окружение запуска. `CARGO_BUILD_JOBS` может отсутствовать
или задавать другой параллелизм; упаковщик не считает его частью Android-профиля.

Сначала соберите host-инструмент без target `RUSTFLAGS` и подготовьте общий staging:

```sh
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_DEBUG=0
cargo build -p skvoz-core-tools --release --locked --offline
export CORE_TOOLS="$PWD/target/release/skvoz-core-tools"
export CORE_NDK_ARCHIVE=/absolute/archives/android-ndk-r30-linux.zip
export CORE_SHARED_TOOLS=/absolute/prepared/core-android-tools
"$CORE_TOOLS" prepare-android "$CORE_NDK_ARCHIVE" "$CORE_SHARED_TOOLS"
```

`CORE_NDK_ARCHIVE` — существующий закреплённый ZIP, `CORE_SHARED_TOOLS` — отдельный
каталог результата. Команда проверяет NDK, получает только недостающие закреплённые
Rust std archives по HTTPS и выборочно распаковывает regular files; относительные
symlinks NDK проверяются до создания и после разрешения. Пределы распаковки —
1200 MiB NDK и384 MiB на std target. Результат включает `sdk/ndk/30.0.16248370`,
`rust-android/`, `downloads/` и `tooling.json`. Готовый staging не перезаписывается;
повторные сборки используют эти же инструменты read-only и пропускают подготовку.

Затем задайте target environment:

```sh
export SKVOZ_CORE_ANDROID_NDK="$CORE_SHARED_TOOLS/sdk/ndk/30.0.16248370"
export SKVOZ_CORE_ANDROID_SYSROOT="$CORE_SHARED_TOOLS/rust-android"
export SKVOZ_CORE_ANDROID_NDK_ARCHIVE="$CORE_NDK_ARCHIVE"
export CORE_STD_ARCHIVES="$CORE_SHARED_TOOLS/downloads"
export RUSTFLAGS="-C link-arg=-Wl,-z,max-page-size=16384 --sysroot $SKVOZ_CORE_ANDROID_SYSROOT"
unset CARGO_ENCODED_RUSTFLAGS
export CORE_NDK_BIN="$SKVOZ_CORE_ANDROID_NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$CORE_NDK_BIN/aarch64-linux-android31-clang"
export CC_aarch64_linux_android="$CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"
export AR_aarch64_linux_android="$CORE_NDK_BIN/llvm-ar"
export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$CORE_NDK_BIN/x86_64-linux-android31-clang"
export CC_x86_64_linux_android="$CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER"
export AR_x86_64_linux_android="$CORE_NDK_BIN/llvm-ar"
```

## Отдельные outputs

`CORE_ARTIFACT` каждый раз указывает на новый каталог. `package-android`
проверяет выбранные pins и текущие переменные среды, ELF64/machine, экспорт
ABI v1, 16 KiB LOAD alignment и системную dependency closure. Допускаются только
`libc.so`, `libm.so`, `libdl.so`, `liblog.so`; GLIBC и дополнительные зависимости
отвергаются. Упаковка не пересобирает переданную библиотеку. Записанные build
inputs должны совпадать со средой её фактической сборки; ELF-проверка не заменяет
происхождение исходников и инструментов.

```sh
export CORE_TARGET=aarch64-linux-android
export CORE_ARTIFACT="$PWD/target/core-artifact-arm64-v8a"
export SKVOZ_CORE_ANDROID_STD_ARCHIVE="$CORE_STD_ARCHIVES/rust-std-1.92.0-aarch64-linux-android.tar.xz"
cargo build -p skvoz-core-abi --target "$CORE_TARGET" --release --locked --offline
"$CORE_TOOLS" package-android "$CORE_TARGET" \
  "target/$CORE_TARGET/release/libskvoz_core.so" "$CORE_ARTIFACT"

export CORE_TARGET=x86_64-linux-android
export CORE_ARTIFACT="$PWD/target/core-artifact-x86_64"
export SKVOZ_CORE_ANDROID_STD_ARCHIVE="$CORE_STD_ARCHIVES/rust-std-1.92.0-x86_64-linux-android.tar.xz"
cargo build -p skvoz-core-abi --target "$CORE_TARGET" --release --locked --offline
"$CORE_TOOLS" package-android "$CORE_TARGET" \
  "target/$CORE_TARGET/release/libskvoz_core.so" "$CORE_ARTIFACT"
```

Manifest сохраняет target, ABI, SHA256, SDK/source inventory, dependency closure,
SONAME, target profile и фактические версии инструментов. Android-профиль не
получает Linux GLIBC identity; metadata bounds 1024/1024 проверяются compile-time
assertions поставщика и остаются консервативными пределами.

## Передача Android consumer

Потребитель получает `sdk/`, `artifact.json` и `lib/libskvoz_core.so` для каждого
target. `SKVOZ_CORE_ARTIFACT="$CORE_ARTIFACT"` выбирает готовую библиотеку для
соответствующего JNI build; binding проверяет target/ABI/digest и не собирает Core.
Android-приложение включает `libskvoz_core.so` вместе с JNI-библиотекой в каталог
каждого ABI. Владелец приложения проверяет loader closure, JNI lifecycle,
свойства APK и загрузку с 16 KiB pages. Cross-compile и упаковка Core сами по себе
эти проверки не подтверждают. Linux artifact остаётся отдельным output.

Три target-архива и публикация из owning commit описаны в [релизах Core](releases.md).
CI использует те же `prepare-android` и `package-android`, сохраняя эти pins.
