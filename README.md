# SKVOZ Core

Универсальный Rust engine двунаправленных непрозрачных байтовых потоков, NATS
runtime и отдельный daemon с IPC v1. Один Core используется всеми ролями SKVOZ.

| Каталог | Назначение |
| --- | --- |
| `src/core/` | Sans-I/O Stream/Manager и NATS runtime поставщика |
| `src/contracts/` | Общие типы, проверки профиля и формулы памяти без engine |
| `src/tls/` | Единый проверенный TLS verifier для runtime и enrollment |
| `src/abi/` | Поставщик `libskvoz_core.so`, C ABI v1 |
| `src/binding/` | Тонкий Rust consumer готовой библиотеки |
| `src/daemon/` | Самостоятельный daemon и Unix IPC v1 |
| `src/connectors/tcp/` | Ограниченный пример TCP relay |
| `src/clients/` | Примеры IPC на Python и Ruby |
| `src/test-support/` | Общая закрытая TLS/NATS fixture для тестов компонентов |
| `src/testbench/` | Самостоятельный CLI стенда: demo, load и runtime-worker |
| `src/include/` | Публичный C header ABI v1 |
| `src/tools/` | Rust-инструмент упаковки SDK, библиотек и релизов с его тестами |
| `docker/` | Dockerfile сборки и тестов, профили Android |

Все собственные исходники, примеры и тесты находятся в `src/`; рецепты сборки — в `docker/`
и `.github/workflows/`. Cargo workspace, lock и toolchain находятся в `src/`;
Cargo запускается оттуда и создаёт `src/target/`. Docker/Buildx запускается из
корня репозитория. Тесты принадлежат проверяемым компонентам: engine/runtime —
`src/core/tests/`, потребитель artifact — `src/binding/tests/`, IPC — `src/daemon/`.
Экспортируемый SDK содержит `contracts/`, `tls/`, `binding/`
и `include/` независимо от размещения исходников в репозитории.

Нужен Rust 1.92.0. Зависимости закреплены в `src/Cargo.lock`; patched async-nats
0.50.0 хранится в `src/vendor/async-nats`, его происхождение и изменения описаны
в [описании патчей](src/vendor/async-nats/PATCHES.md). Собственный код распространяется
по [MIT](LICENSE), async-nats — по [Apache-2.0](src/vendor/async-nats/LICENSE).

Основной путь поставки — push в `main` → GitHub Actions → точный GitHub Release
→ Actions потребителя с закреплёнными URL/SHA256. Имена трёх target-архивов,
commit-теги и получение релиза описаны в [инструкции релизов](docs/releases.md).
Workflow содержит build и publish; команды сборки и обязательные тесты находятся
в [Dockerfile](docker/rust/Dockerfile). Независимые `linux-artifacts` и
`android-artifacts` выдают платформенные candidates; `artifacts` объединяет все
targets и release-проверки в полный bundle. [Граф и команды](docs/releases.md).

## Локальная сборка полного набора

С Docker и Buildx из корня репозитория:

```sh
sh docker/local_build.sh
```

Скрипт запускает тот же `artifacts`, что и CI: сборку Linux и обоих Android
outputs, обязательные тесты и упаковку. Нужен чистый owning commit; по умолчанию
результат находится в `temp/core-release`, первый аргумент задаёт другой output.
Ошибки сборки и тестов завершают команду. Публикация выполняется отдельно.

## Локальные команды поставщика

Для Linux нужны C linker, GNU `readelf` и `sha256sum`; для функциональных
тестов — `nats-server` 2.15.1 и OpenSSL (их предоставляет Dockerfile). `CORE_ARTIFACT` — новый
каталог результата; инструмент отказывается перезаписывать существующий каталог.
Прямые команды требуют подготовленных инструментов; полную среду создаёт
`sh docker/local_build.sh`.
GitHub Actions использует ресурсы runner; параметры локальной машины не входят
в условия сборки релиза.

```sh
cd src
export CORE_ARTIFACT="$PWD/target/core-artifact"
cargo check -p skvoz-core --no-default-features --lib --locked
cargo test -p skvoz-core --features fault-injection --locked -- --test-threads=1
cargo build -p skvoz-core-abi -p skvoz-core-tools --release --locked
./target/release/skvoz-core-tools package \
  target/release/libskvoz_core.so "$CORE_ARTIFACT"
```

Это явная сборка поставщика. Потребители используют подготовленный artifact;
отсутствующая библиотека, неверные digest/target/ABI завершают сборку ошибкой.
Переменная `SKVOZ_CORE_ARTIFACT` указывает на каталог с `artifact.json` и
`lib/libskvoz_core.so`. Для запуска задаётся путь загрузчика к этому же `lib/`.

```sh
SKVOZ_CORE_ARTIFACT="$CORE_ARTIFACT" cargo build -p skvoz-daemon -p skvoz-tcp --release --locked
LD_LIBRARY_PATH="$CORE_ARTIFACT/lib" ./target/release/skvoz-core-daemon --help
```

[Бинарная граница](docs/binary-abi.md) определяет владение, отмену и ограничения.
Готовность конкретного target подтверждается отдельно; наличие source/binding
не объявляет платформу квалифицированной. Порядок проверки Linux и потребителей
описан в [инструкции сборки](docs/getting-started.md); два явных Android-профиля,
закреплённые инструменты и команды — в [Android supplier](docs/android-supplier.md).

## Контракты

[Движок и память](docs/stream-engine.md), [NATS runtime](docs/nats-runtime.md),
[runtime protocol](docs/runtime-protocol.md), [wire](docs/wire.md),
[daemon и IPC](docs/daemon-ipc.md). Верхнеуровневая документация проекта находится в
[skvoz-docs](https://github.com/skvoz-project/skvoz-docs).

Core отвечает за порядок, кредит, ограниченные очереди, half-close и lifecycle.
Приложение отвечает за маршрутизацию, учётные данные и свою политику соединений.
