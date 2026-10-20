# Проверки Core

Rust-сценарии проверяют Stream/Manager и NATS runtime Core. Версия package1.2.0.
Описание подготовки fixture и прямые команды
находятся в [руководстве сборки](../docs/getting-started.md).

`node`, `mesh`, `process_workload`, `scenarios` и `runtime_scenarios` содержат
сценарии Core. HTTP monitor наблюдается ограниченным Rust-клиентом.
`runtime_delivery` использует явный профиль Core без зависимости от Network.
Смешанные Network проверки принадлежат репозиторию
[skvoz-network](https://github.com/skvoz-project/skvoz-network).

TCP-сценарии находятся в `connectors/tcp/tests/real_nats.rs`; они вызывают готовую
библиотеку через binding. Source/fault/restart/ACL сценарии требуют своей
подготовленной fixture; обычная проверка без неё не заявляет их выполнение.
