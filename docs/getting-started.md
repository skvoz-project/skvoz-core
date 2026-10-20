# Сборка и проверки Core

Основной канал готовых outputs — [GitHub Releases](releases.md): Actions Core
собирает owning commit, Actions потребителя получает закреплённые архивы. Ниже
приведена локальная сборка поставщика и проверка подготовленного artifact.

Rust 1.92.0 и Cargo.lock закрепляют исходные зависимости. Для реальных TLS/NATS
тестов нужны NATS Server 2.15.1 и OpenSSL в PATH. Доступ к производственному
брокеру не нужен: `binding/tests/native.rs` создаёт закрытый временный каталог,
сертификаты и локальный NATS, после проверки удаляет процессы и файлы.

## Поставщик

Для Linux нужны C linker, GNU `readelf` и `sha256sum`; для упаковки Android
действуют отдельные [профили и prerequisites](android-supplier.md).
Ниже используется стандартный `target/` Cargo. Выполняйте команды в
подготовленной ограниченной среде; выбирайте libc поставщика, совместимую
с целевым окружением потребителя.

```sh
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

`binding/tests/native.rs` проверяет реальные handshake, бинарные DATA, отказ
допуска DATA, half-close, wake, отмену и проверку сертификата. Подтверждение
потребления выполняется только после освобождения выданного буфера.

## Независимые процессы runtime

`testbench/` содержит Rust-сценарии Core. Его `real-nats` сценарии
ожидают явно подготовленную ограниченную TLS/auth fixture и переменные
`SKVOZ_NATS_URL`, `SKVOZ_NATS_CA`, `SKVOZ_NATS_RUN_TOKEN`,
`SKVOZ_NATS_P0_PASSWORD` и пароли участвующих peers. Fault/restart/ACL сценарии
дополнительно требуют указанные в исходниках fixture inputs. Они не запускаются
обычной проверкой и не подключаются к произвольному брокеру автоматически.

```sh
cargo test -p skvoz-core --features nats --lib --locked
cargo test -p skvoz-core --features nats --test continuous_nats --locked -- --ignored --test-threads=1
cargo test -p skvoz-testbench --features real-nats --locked -- --test-threads=1
```

Последние две команды предназначены для соответствующей подготовленной fixture;
наличие сценария в дереве не подтверждает его выполнение на новом artifact.
Сценарии TCP находятся в `connectors/tcp/tests/real_nats.rs` и используют готовый
Core через binding. Python/Ruby под `clients/` — примеры IPC consumers, а не
язык инфраструктуры тестов.

## Нагрузочный эксперимент

Rust load mode в testbench является инструментом поставщика. Запускайте его
для явно выбранной гарантии, с конечным deadline и измеренным cap ресурсов.
