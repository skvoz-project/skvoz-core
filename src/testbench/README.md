# Стенд Core

`skvoz-testbench`1.2.0 сохраняет самостоятельные режимы `demo`, `load` и
`runtime-worker`. Подготовка fixture и прямые команды описаны в
[руководстве сборки](../../docs/getting-started.md).

Контракты Stream/Manager, NATS и runtime принадлежат [тестам Core](../core/tests/)
и выполняются обычным CI с собственными локальными fixtures. Стенд владеет
своими helpers, CLI и `process_workload`; source-модулей из Core tests нет.
HTTP monitor использует ограниченный Rust-клиент.

Режимы `demo`/`load` проверяют статический NatsNode, а `runtime-worker` —
NatsRuntime. Они требуют явно подготовленной внешней fixture и не заменяют
функциональные CI-контракты или измерения production Network/Server.

Три теста TCP relay находятся в `src/tcp/tests/real_nats.rs`: это
контракт поддерживаемого bounded example, который использует готовую библиотеку
через binding. Смешанные Network проверки принадлежат
[skvoz-network](https://github.com/skvoz-project/skvoz-network).
