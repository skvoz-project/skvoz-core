# SKVOZ Core

`skvoz-core` — универсальная Rust-библиотека двунаправленных байтовых потоков.
Все коннекторы и обе стороны используют одну реализацию и контракт.
По умолчанию библиотека содержит Stream/Manager и кодек без I/O и внешних
зависимостей. Возможность `nats` добавляет Tokio/async-nats и два способа встраивания:

- `NatsRuntime` — динамические сессии, присоединение, проверка живости и восстановление для новых потоков.
- `NatsNode` — низкоуровневые статические маршруты для транспортных проверок; приложения и TCP relay используют NatsRuntime.

`stream.rs` владеет состоянием одного потока, `peer_credit.rs` — суммарными
разрешениями DATA peer, `stream_manager.rs` — владением потоками, admission и
бюджетами нескольких peers. Статический `NatsNode` находится в `nats_node.rs`,
динамические сессии и транспорт — в `runtime.rs` и `runtime/`.
`tls_config.rs` строит проверенную TLS-конфигурацию; `nats_fixture.rs` владеет
общей TLS/NATS fixture. `Cargo.toml` задаёт путь `lib.rs` явно.
Публичные типы и проверка профиля без I/O принадлежат `skvoz-core-contracts`;
`runtime.rs` реэкспортирует их через прежний `skvoz_core::runtime`.
В `runtime/` delivery allocator и чистая credit-модель разделены между
`delivery_allocator.rs` и `delivery_credit.rs`; `close_handshake.rs` содержит
terminal handshake, `session_close.rs` — его координацию в runtime, а
`publication_counters.rs` — счётчики публикаций.
`tests/` — контракты, реальные NATS/runtime suite и [публичные векторы](tests/fixtures/README.md);
`tests/unit/` содержит приватные дочерние модули тестов;
`examples/` — обмен в памяти и встраивание runtime с профилем.

| Что найти | Контракт |
| --- | --- |
| Stream/Manager, состояния, кредит и память | [Движок](../docs/stream-engine.md) |
| Присоединение, восстановление, доверие и очереди | [NATS runtime](../docs/nats-runtime.md) |
| Бинарные пакеты | [Wire v2](../docs/wire.md) |
| Доступ из других языков | [Демон и IPC v1](../docs/daemon-ipc.md) — тот же Core внутри отдельного процесса |

Из каталога `src/` репозитория:

```sh
cargo check -p skvoz-core --no-default-features --lib --locked
cargo test -p skvoz-core --features fault-injection --locked -- --test-threads=1
```

Для функциональных NATS-сценариев нужны `nats-server` 2.15.1 и OpenSSL. Тесты
сами создают временные сертификаты, ACL и локальные процессы брокера, в том числе
для restart/reload; внешний NATS, Docker socket и переменные стенда не нужны.
`fault-injection` включает необходимые тестовые hooks и не входит в production
сборку ABI с feature `nats`. Обычный CI выполняет эти сценарии; тот же полный
путь доступен из корня репозитория через `sh docker/local_build.sh`.

Чистые state/credit/wire contracts, portable traces и binary vectors остаются
детерминированными. Реальные Runtime-тесты проверяют bytes/consume, ownership,
бюджеты, replay, recovery и отказы; компактный `static_nats` сохраняет отдельный
контракт публичного NatsNode. TLS-матрица проверяется через поставляемую binary
границу в [руководстве сборки](../docs/getting-started.md). `tests/support/`
содержит только Core test drivers; стенд владеет своими CLI helpers.

Коннекторы отвечают за внешний I/O, метаданные, выдачу и хранение учётных данных,
сертификаты и интерфейс приложения. Core поддерживает проверенное доверие
System/ManagedCa и подготовленные логин/пароль. Интерфейсы и wire остаются
экспериментальными; C ABI v1 описан отдельно и требует квалифицированного artifact.
[Бинарная граница](../docs/binary-abi.md) и [карта исходников](../README.md).
