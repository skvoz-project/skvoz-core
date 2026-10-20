# Релизы Core

Core поставляет готовую `libskvoz_core.so` и Rust/C SDK для трёх отдельных targets:
`x86_64-unknown-linux-gnu`, `aarch64-linux-android` и `x86_64-linux-android`.
Версия Core остаётся `5.0.0`, C ABI — `1`. Android-профили и закреплённые
NDK/Rust инструменты описаны в [Android supplier](android-supplier.md).
Проверка Android loader, JNI и APK принадлежит потребителю.

## Автоматическая поставка

Workflow [Core](../.github/workflows/core.yml) выполняет Linux-проверки и
последовательную сборку двух Android targets. Каждый supplier работает в отдельном
контейнере закреплённого Rust 1.92.0: 2 CPU, 3 GiB RAM, swap 0 и PID 256,
без Linux capabilities, с `no-new-privileges` и конечным deadline. Checkout
доступен контейнеру только для чтения. После обоих suppliers read-only job
`tools-contracts` получает три CI-архива этого же запуска и выполняет постоянные
unit-, export- и release-contract тесты из `tools/`. Она собирает только host-tools;
библиотеки Core повторно не собираются. Фикстуры ограничены тремя распакованными
target-каталогами. Проверяются упаковка готовых bytes, SDK/source inventory,
отказы по неверной identity/digest, публикация через mock GitHub и очистка
контейнера. Ошибка supplier или этих тестов блокирует релиз.

Pull request выполняет те же Core-tools контракты и получает временные CI artifacts. Push в `main` или ручной
запуск `workflow_dispatch` для `main` в `skvoz-project/skvoz-core` дополнительно
запускает отдельную публикацию. Только эта job имеет `contents: write`;
`GH_TOKEN` передаётся только шагу публикации.

Тег содержит полный SHA именно того commit, из которого собраны outputs:

```text
core-v5.0.0-<40-символьный-SHA>
```

Релиз содержит четыре assets:

- `skvoz-core-5.0.0-x86_64-unknown-linux-gnu.tar.gz`;
- `skvoz-core-5.0.0-aarch64-linux-android.tar.gz`;
- `skvoz-core-5.0.0-x86_64-linux-android.tar.gz`;
- `SHA256SUMS` с SHA256 всех трёх архивов.

Архив имеет плоский корень: `artifact.json`, `lib/libskvoz_core.so`, `sdk/`,
`source-inputs.sha256`, `LICENSE-async-nats`, `PATCHES-async-nats.md`.
Linux также содержит `bin/skvoz-core-daemon`. Собственная MIT-лицензия находится
в `sdk/LICENSE`. `artifact.json` сохраняет target, ABI, версии инструментов,
библиотечный digest, SDK/source inventory hashes и `source.revision` с полным SHA.
Исходники supplier не входят в архив; их inventory связывает output с owning commit.

Перед публикацией инструмент заново проверяет все три архива, manifests,
библиотечные digest, полное множество файлов SDK и `SHA256SUMS`. Тег создаётся атомарно с указанным SHA, затем публикация требует его наличия
через `--verify-tag`. Уже существующий тег, в том числе появившийся при
одновременном запуске, завершает запуск ошибкой. Повторный запуск не дополняет существующий релиз,
не заменяет assets и не передвигает тег. Релиз не помечается `latest`.
При частичном сбое публикации нужна отдельная проверка состояния GitHub;
автоматического удаления или исправления существующего релиза нет.

## Явная упаковка

Обычные команды `package` и `package-android` сохраняют локальный candidate с
`source.revision: null`. Для релизной сборки checkout должен быть чистым, а
`SKVOZ_CORE_SOURCE_REVISION` — совпадать с его полным `HEAD`:

```sh
export SKVOZ_CORE_SOURCE_REVISION="$(git rev-parse HEAD)"
# Выполните команды сборки/упаковки Linux и Android из инструкций supplier.
./target/release/skvoz-core-tools archive "$LINUX_ARTIFACT" "$SKVOZ_CORE_SOURCE_REVISION" "$RELEASE_DIRECTORY"
./target/release/skvoz-core-tools archive "$ANDROID_ARM64_ARTIFACT" "$SKVOZ_CORE_SOURCE_REVISION" "$RELEASE_DIRECTORY"
./target/release/skvoz-core-tools archive "$ANDROID_X64_ARTIFACT" "$SKVOZ_CORE_SOURCE_REVISION" "$RELEASE_DIRECTORY"
./target/release/skvoz-core-tools checksums "$RELEASE_DIRECTORY" "$SKVOZ_CORE_SOURCE_REVISION"
```

Передайте переменную ревизии до упаковки всех трёх targets. Команды не меняют
готовые native bytes и не перезаписывают существующие outputs. Нельзя присваивать
старому локальному candidate новый SHA: библиотека, SDK и inventory должны быть
получены из указанного commit. `publish` предназначен для доверенной GitHub job
и дополнительно проверяет repository, event, `main` и `GITHUB_SHA`.

## Получение определённого релиза

Выберите существующий точный тег, без поиска последнего релиза:

```sh
CORE_TAG='core-v5.0.0-<полный-SHA>'
mkdir core-download
cd core-download
gh release download "$CORE_TAG" --repo skvoz-project/skvoz-core \
  --pattern '*.tar.gz' --pattern SHA256SUMS
sha256sum --check --strict SHA256SUMS
```

Потребитель сохраняет точный release URL и SHA256 выбранного архива, проверяет
внутренние manifests, owning repository/revision, target/ABI, SDK inventory и
библиотеку, затем устанавливает готовые bytes. Отсутствие архива или ошибка
проверки завершают обновление; скрытой пересборки Core нет. Автоматические
команды обновления pins принадлежат соответствующему репозиторию потребителя.
До первого опубликованного релиза локальные pins остаются локальными; URL и
будущие hashes появляются после фактической публикации.
