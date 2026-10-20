# Минимальный TCP relay

Экспериментальный Rust package `skvoz-tcp` в репозитории Core. Владеет одним
TCP-сокетом и небольшими I/O буферами; использует ту же библиотеку
[Core](../../core/README.md), что и другие коннекторы. Маршрутизация и
flow control находятся в Core.

`relay_one(&mut NatsRuntime, RuntimeKey, TcpStream, Duration)` получает уже открытый
Core stream и выделенный для этого сокета NatsRuntime с непрерывным protocol3. Host
создаёт сокет, настраивает authenticated membership, ждёт peer_ready и принимает OPEN. Relay передаёт
binary bytes в обоих направлениях; TCP EOF вызывает FIN, а RemoteFinished —
shutdown отправляющего направления сокета. Ответ после EOF запроса допустим.

Relay хранит не более 1024 байт ожидающей отправки и один выданный Core DATA
chunk (не больше настроенного max_frame). Кредит возвращается только после
полной записи и освобождения chunk. Общий timeout охватывает и awaited I/O.
При ошибке или deadline relay освобождает буферы, отменяет Core stream и даёт
до 250 мс на передачу CANCEL/обработку terminal work, сохраняя исходную ошибку.
Это bounded попытка доставки, а не гарантия подтверждения отмены удалённым peer.
Если bounded попытка не завершилась, relay локально fail-close peer и выдаёт
terminal events, освобождая его reservation. Отмена output future навсегда
вызывает conservative transport failure. Runtime проверяет nonce/watermark liveness;
будущие перезапуски relay не возобновляют старый поток.

Воспроизводимый real TCP пример и failure checks запускаются из корня:

```sh
cargo test -p skvoz-tcp --release --locked --test real_nats -- --test-threads=1
cargo test -p skvoz-core --features nats --lib --locked
```

Перед командами задайте `SKVOZ_CORE_ARTIFACT` и `LD_LIBRARY_PATH` по
[инструкции сборки](../../docs/getting-started.md#потребитель). Тесты создают
собственную закрытую TLS/NATS fixture; нужны NATS Server2.15.1 и OpenSSL в PATH.
Happy path проверяет 32 768 байт запроса, FIN, затем 65 536 байт ответа и EOF.
Остальные owning тесты проверяют настоящий socket write error и общий deadline с получением
CANCEL через NATS и освобождением фактических ресурсов обоих nodes.

Это пример одного соединения. Приём множества сокетов, политика назначения
и протокольные адаптеры находятся за его границами. Для нескольких сокетов
приложению нужен отдельный диспетчер и подходящий runtime. Прерванный поток
не возобновляется; новый требует нового экземпляра node.
