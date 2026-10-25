# Бинарный интерфейс Core v1

Статус: реализованная граница поставки; проверка готовых артефактов выполняется
для каждого target и окружения отдельно. Версия Core сохраняется 5.0.0, wire/runtime/IPC не меняются.

`libskvoz_core.so` предоставляет C ABI v1 для NATS runtime. Sans-I/O
`Stream`/`Manager` остаются явным исходным Rust API поставщика; они не объявляются
частью бинарного ABI. Обычные Network, daemon и TCP используют готовую библиотеку
и небольшой `skvoz-core-binding`; `skvoz-core-contracts` содержит только DTO,
проверки профиля и формулы памяти. `src/tls_config.rs` содержит единственную
исходную реализацию проверенного доверия TLS. Core использует её как приватный
модуль, адаптеры enrollment — как самостоятельный экспортируемый SDK crate
`skvoz-core-tls`.

## Владение и вызовы

Runtime и асинхронная операция представлены непрозрачными handle. Владелец вызывает
их последовательно; параллельное использование, повторное уничтожение и повторный
вход из callback запрещены. Разные runtime независимы. Один runtime допускает
только одну активную операцию connect/turn/shutdown. Handle уничтожает поставщик.
Конфигурация и холодные наблюдения кодируются ограниченными JSON DTO; DATA, send,
metadata и event payload передаются сырыми байтами с явной длиной, без base64/JSON.

Входные срезы действуют только в течение вызова. Выданные payload принадлежат
поставщику и освобождаются его `buffer_free`; binding хранит этот allocator вместе
с буфером. Чтение события не подтверждает потребление. Вызывающий освобождает
полученный backing до `consume_through`. Callback допуска DATA синхронный, без
повторного входа; отказ оставляет DATA у Core и сохраняет прогресс иных событий.
Пакет ограничен прежним `MAX_BATCH_EVENTS = 256`, DATA — runtime frame 16384 байт,
metadata — runtime profile до 512 байт; холодный request/result — 64 KiB.

## Асинхронность и отмена

Библиотека содержит один общий Tokio executor с одним I/O worker и ограниченным
blocking pool; очереди команд или задачи на поток не добавляются. Существующие
задачи NATS и их ограниченные очереди сохраняются. Операция хранит исходный future
Core. `operation_poll` принимает сохраняемый wake callback с retain/release
контекста; уведомление лишь будит executor владельца, не выполняет вызовы Core.

Host wake для `turn_with_wake` опрашивается поставщиком только на месте исходного
idle wait: после завершения guarded output, с приоритетом входящего NATS.
Внешняя готовность никогда не отменяет извлечённый output. Drop/cancel операции
уничтожает исходный future с его cancellation guards; runtime сохраняется,
следующий вызов наблюдает transport fence при незавершённой отправке. Connect
при отмене не возвращает частично подключённый runtime. Shutdown имеет прежний
конечный deadline. Никаких скрытых retry или изменений deadline в binding нет.

## Ошибки, версии и доставка

Статусы ABI различают OK, PENDING, invalid argument, busy, unsupported ABI и panic;
ошибки Core передаются существующими типами без секретов. Все exports удерживают
unwind; panic помечает затронутый handle неисправным, последующие вызовы отвергаются.
Произвольные повреждённые указатели не являются поддерживаемым входом C API.

Binding проверяет ABI major/capabilities до создания runtime. При отсутствии,
несовместимости или неверном digest артефакта сборка/загрузка завершается ошибкой.
Путь библиотеки — явный build input; исходники Core не собираются по fallback.
Профиль поставщика фиксирует target, compiler/toolchain, lock, features,
минимальную системную ABI, SHA256 библиотеки и транзитивную runtime closure.

## Бюджет моста

Binding копирует сырой payload в `Box<[u8]>` allocator потребителя. В каждый
момент активен один callback; дополнительный временный backing — максимум
16384 байта на runtime. Supplier освобождает исходный payload после callback.
`metadata_backing()` сохраняет исходный смысл sans-I/O engine;
`metadata_backing_with_abi()` добавляет этот предел и `ABI_COLD_BRIDGE_BACKING = 65536`
для ограниченной сериализации DTO к профилю native consumer. Encoder прекращает
запись на пределе, не создавая предварительную неограниченную копию.
`ABI_EXECUTOR_STACK_BACKING = 1572864` резервирует один worker и максимум два
blocking stack по 512 KiB один раз на процесс. Это учёт backing, не новый RSS SLA.
Linux x86_64 GNU использует проверенные Entry592/Peer656; остальные исходные
профили используют консервативные пределы1024/1024 с compile-time проверкой
поставщика `actual <= bound`. Это не квалификация Android/Windows binary ABI.

Репозиторий текущей реализации: [skvoz-core](https://github.com/skvoz-project/skvoz-core).
Поле `source` в artifact manifest содержит `repository`, `revision` и
`tree_sha256`. `repository` и `owning_repository` указывают на этот репозиторий;
`tree_sha256` совпадает с `source_inputs_sha256`: это SHA256 отсортированного
`source-inputs.sha256` с точными входными путями и хешами. Упаковщик создаёт
candidate с `revision: null`; опубликованный release должен указывать commit
репозитория поставщика. `sdk_manifest_sha256` отдельно фиксирует `sdk/sdk.sha256`.
SDK содержит только контракты, TLS, binding, C header и лицензию MIT.

Исходники SDK находятся в `src/`, упаковщик — в `src/tools/`. Source inventory
охватывает `.dockerignore` и деревья `src/` и `docker/` (включая `src/vendor/`).
Workspace manifest, lock и toolchain входят один раз по путям
`src/{Cargo.toml,Cargo.lock,rust-toolchain.toml}`; `lock_sha256` фиксирует
`src/Cargo.lock`. Генерируемый `src/target/` исключён из source inventory.
Сценарии testbench и IPC-примеры входят в идентичность
исходников; их запуск не становится требованием сборки или поставки.
Внутри SDK сохраняются каталоги `contracts/`, `tls/`, `binding/` и `include/`.

Android supplier выбирает только два [явных профиля](android-supplier.md).
Их pins и проверки упаковки не меняют ABI v1 или семантику runtime и не
квалифицируют Android загрузку/JNI. Потребитель связывается с библиотекой своего
target; source SDK общий, native bytes и platform evidence раздельные.

## Поля вызовов

`capabilities` равен7: bit0 — runtime, bit1 — event admission, bit2 — host wake.
`start` kind0 принимает JSON массива `[RuntimeConfig, ManagerConfig]`; точные
поля и сериализуемые варианты определяет исходный пакет contracts версии5.0.0.
Эта схема входит в ABI v1. kind1 запускает turn с `wait_ms`, kind2 — shutdown.
Повторный poll завершённой операции недопустим; освободите operation отдельно.

Для JOIN/REVOKE/TERMINATE/PEER_STATUS/PEER_READY/PEER_DRAINED аргумент — PeerId.
AUTHORIZE использует PeerId в аргументе и generation в двух полях epoch;
`key.incarnation` равен0 для None и1 для Some. Some(0) сохраняет исходный отказ.
OPEN принимает PeerId и raw metadata; ACCEPT/REJECT — полный key и raw metadata.
SEND принимает key и максимум16384 raw bytes, возвращает принятую длину в value
или WOULD_BLOCK. CONSUME использует абсолютный offset в аргументе. Остальные
операции потока принимают полный key. SET_DIAGNOSTICS использует аргумент0/1.
EPOCH возвращает две половины128-bit значения в reply.key; холодные наблюдения
возвращают ограниченный JSON в reply.buffer, ошибки Core — такой же error DTO.
Значение reply.buffer освобождается supplier buffer_free даже при Core error.

Event kinds:0 IncomingOpen,1 Opened,2 Rejected,3 Data,4 Writable,5 RemoteFinished,
6 Closed. Payload заимствован до возврата receive callback. Closed reason:
0 Finished,1 Rejected,2 Cancelled,3 TransportLost,4 ProtocolError,5 OpenTimeout,
6 ResourcePressure. Это native ABI codes, отдельно от wire/IPC codes.
