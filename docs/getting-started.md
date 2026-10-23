# Сборка и проверки Core

Основной канал готовых outputs — [GitHub Releases](releases.md): Actions Core
собирает owning commit, Actions потребителя получает закреплённые архивы. Ниже
приведена локальная сборка поставщика и проверка подготовленного artifact.

`src/rust-toolchain.toml` с Rust 1.92.0 и `src/Cargo.lock` закрепляют исходные зависимости. Для реальных TLS/NATS
тестов нужны NATS Server 2.15.1 и OpenSSL в PATH. Доступ к производственному
брокеру не нужен: `src/binding/tests/native.rs` создаёт закрытый временный каталог,
сертификаты и локальный NATS, после проверки удаляет процессы и файлы.

## Поставщик

Для Linux нужны C linker, GNU `readelf` и `sha256sum`; для упаковки Android
действуют отдельные [профили и prerequisites](android-supplier.md).
Все Cargo-команды ниже выполняются из `src/`; стандартный output — `src/target/`.
Из корня сначала перейдите в `src/`. Выполняйте команды в
подготовленной ограниченной среде; выбирайте libc поставщика, совместимую
с целевым окружением потребителя.

```sh
cd src
export CORE_ARTIFACT="$PWD/target/core-artifact"
cargo test -p skvoz-core --no-default-features --locked
cargo build -p skvoz-core-abi -p skvoz-core-tools --release --locked
./target/release/skvoz-core-tools package \
  target/release/libskvoz_core.so "$CORE_ARTIFACT"
```

`CORE_ARTIFACT` должен указывать на новый каталог. В нём появляются готовая
библиотека, manifest с SHA256/ABI/target/зависимостями, точный исходный SDK,
лицензии и source-input hashes. Это candidate до относящихся проверок.
Инструмент `sdk "$OUTPUT"` создаёт только исходный SDK без engine в новом каталоге:
`contracts/`, `tls/`, `binding/`, `include/`, `LICENSE` и проверяемый `sdk.sha256`.
Упаковка фиксирует фактические `DT_NEEDED`, SONAME (или null) и требуемые GLIBC
symbols. Linux-команда требует `libc.so.6` и реальные GLIBC version references;
библиотека Android x86_64 отвергается до создания output. Совпадение target triple
само по себе не доказывает совместимость libc:
проверьте загрузку в целевой среде и её зависимости отдельно.

## Потребитель

```sh
export SKVOZ_CORE_ARTIFACT="$CORE_ARTIFACT"
export LD_LIBRARY_PATH="$CORE_ARTIFACT/lib"
cargo test -p skvoz-core-binding -p skvoz-daemon -p skvoz-tcp --release --locked
cargo build -p skvoz-daemon --release --locked
```

Binding проверяет компонент, версию 5.0.0, ABI v1, target и SHA256 до линковки.
Неверный или отсутствующий artifact завершает команду ошибкой. Он никогда не
запускает сборку Core. Для самостоятельного запуска daemon установите
`bin/skvoz-core-daemon` рядом с `lib/libskvoz_core.so`; бинарник использует
`$ORIGIN/../lib`. При запуске тестовых бинарников Cargo задайте
`LD_LIBRARY_PATH="$CORE_ARTIFACT/lib"`.

`src/binding/tests/native.rs` проверяет реальные handshake, бинарные DATA, отказ
допуска DATA, half-close, wake, отмену и проверку сертификата. Подтверждение
потребления выполняется только после освобождения выданного буфера.

## Независимые процессы runtime

`src/core/tests/{a_runtime,continuous_nats,runtime_delivery}.rs` проверяют
NatsRuntime через локальный настоящий NATS. Обычный CI запускает их явно с
`fault-injection`; production `nats`/ABI не содержит этих hooks. Каждый тест
создаёт свои временные сертификаты, ACL/peer credentials и дочерний nats-server,
управляет только его restart/reload и удаляет fixture. Внешние NATS/container и
переменные стенда не нужны. `static_nats` отдельно проверяет публичный NatsNode.

Из `src/`, с `nats-server` 2.15.1 и OpenSSL:

```sh
cargo check -p skvoz-core --no-default-features --lib --locked
cargo test -p skvoz-core --features fault-injection --locked -- --test-threads=1
```

Весь набор поставки выполняется из корня репозитория командой
`sh docker/local_build.sh`: native binding/TLS, daemon startup+IPC, TCP example,
SDK/archive/release contracts и оба Android targets. Сценарии TCP принадлежат
`src/connectors/tcp/tests/real_nats.rs` и используют готовый Core через binding.
Python/Ruby под `src/clients/` — примеры IPC consumers, не инфраструктура тестов.

## Нагрузочный эксперимент

`src/testbench/` сохраняет самостоятельные режимы demo/load/runtime-worker,
со своими helpers, независимо от test fixture Core. Для этих режимов стенда
требуется явно подготовленная внешняя fixture; demo/load используют NatsNode,
а runtime-worker — NatsRuntime.
Rust load mode в testbench является инструментом поставщика. Запускайте его
для явно выбранной гарантии, с конечным deadline и измеренным cap ресурсов.
