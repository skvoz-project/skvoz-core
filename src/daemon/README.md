# Демон Core SKVOZ

`skvoz-core-daemon` — исполняемый файл Linux с той же универсальной
[библиотекой Core](../core/README.md) и `NatsRuntime`. Приложения других языков
используют единый локальный [IPC v1](../../docs/daemon-ipc.md) через закрытый Unix-сокет.
Каталог содержит управление процессом, профилем, сокетом и владельцами IPC.

Из каталога `src/` репозитория с подготовленным `CORE_ARTIFACT`:

```sh
export SKVOZ_CORE_ARTIFACT="$CORE_ARTIFACT"
export LD_LIBRARY_PATH="$CORE_ARTIFACT/lib"
cargo build --release -p skvoz-daemon --locked
./target/release/skvoz-core-daemon --help
cargo test -p skvoz-core-binding -p skvoz-daemon --release --locked
```

Перед сборкой задайте `SKVOZ_CORE_ARTIFACT` на подготовленный Core artifact.
Daemon связывается с готовой библиотекой через binding. Последняя команда
проверяет IPC/config/endpoint и реальную бинарную границу runtime с локальным
TLS/NATS. Она не объявляет выполненными все прежние многопроцессные сценарии
Python/Ruby. [Подготовка artifact и тестов](../../docs/getting-started.md).
`examples/daemon-smoke.rs` — самостоятельная проверка готового daemon с
закрытой fixture: обычный `main`, без libtest, для минимальной поддерживаемой
libc. Пример получает путь к готовому daemon аргументом; Docker tests-linux
запускает его против собранного binary: проверяются TLS/NATS startup, IPC HELLO
и отсутствие credentials в логах. Общая fixture остаётся в `src/test-support/`.

Для своего брокера подготовьте закрытый профиль по
[инструкции](../../docs/daemon-ipc.md#сборка-запуск-и-профиль). `SKVOZ_PROFILE`
должен содержать абсолютный путь к нему:

```sh
./target/release/skvoz-core-daemon --check-config "$SKVOZ_PROFILE"
./target/release/skvoz-core-daemon --config "$SKVOZ_PROFILE"
```

Демон можно запускать как внешнюю службу или управляемый дочерний процесс.
Конфигурация, права файлов, запуск и остановка, бюджеты и бинарный контракт
описаны в [руководстве IPC](../../docs/daemon-ipc.md). Демон передаёт непрозрачные
метаданные и байты; учётные данные, PeerId, сертификаты и политика назначения
принадлежат приложению. Прерванные потоки не возобновляются.

Входящие открытия получает один эксклюзивный обработчик; остальные IPC-сессии
владеют только своими исходящими потоками.
[Векторы](tests/fixtures/README.md), [пример Python](../clients/python/README.md)
и [пример Ruby](../clients/ruby/README.md) позволяют реализовать новый адаптер.
