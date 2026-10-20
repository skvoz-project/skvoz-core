# SKVOZ Core

Универсальный Rust engine двунаправленных непрозрачных байтовых потоков, NATS
runtime и отдельный daemon с IPC v1. Один Core используется всеми ролями SKVOZ.

| Каталог | Назначение |
| --- | --- |
| `core/` | Sans-I/O Stream/Manager и NATS runtime поставщика |
| `contracts/` | Общие типы, проверки профиля и формулы памяти без engine |
| `tls/` | Единый проверенный TLS verifier для runtime и enrollment |
| `abi/` | Поставщик `libskvoz_core.so`, C ABI v1 |
| `binding/` | Тонкий Rust consumer готовой библиотеки |
| `daemon/` | Самостоятельный daemon и Unix IPC v1 |
| `connectors/tcp/` | Ограниченный пример TCP relay |
| `clients/` | Примеры IPC на Python и Ruby |

Нужен Rust 1.92.0. Зависимости закреплены в `Cargo.lock`; patched async-nats
0.50.0 хранится в `vendor/async-nats`, его происхождение и изменения описаны
в [описании патчей](vendor/async-nats/PATCHES.md). Собственный код распространяется
по [MIT](LICENSE), async-nats — по [Apache-2.0](vendor/async-nats/LICENSE).

Основной путь поставки — push в `main` → GitHub Actions → точный GitHub Release
→ Actions потребителя с закреплёнными URL/SHA256. Имена трёх target-архивов,
commit-теги и получение релиза описаны в [инструкции релизов](docs/releases.md).

## Локальные команды поставщика

Для Linux нужны C linker, GNU `readelf` и `sha256sum`. `CORE_ARTIFACT` — новый
каталог результата; инструмент отказывается перезаписывать существующий каталог.
Команды выполняются в подготовленной среде с ограниченными ресурсами.

```sh
export CORE_ARTIFACT="$PWD/target/core-artifact"
cargo test -p skvoz-core --no-default-features --locked
cargo test -p skvoz-core --features nats --locked
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
