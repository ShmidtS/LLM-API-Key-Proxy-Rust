# LLM-API-Key-Proxy Rust

Rust-миграция `LLM-API-Key-Proxy`: HTTP proxy для LLM API с rotation credentials, provider registry, OpenAI-compatible endpoints, Anthropic Messages API, admin endpoints и TUI мониторингом.

Исходный Python проект находился в `E:/456/LLM-API-Key-Proxy`. Текущий Rust workspace находится в `E:/456/LLM-API-Key-Proxy-Rust`.

## Архитектура

Workspace состоит из следующих crates:

| Crate | Назначение |
| --- | --- |
| `proxy_app` | Axum HTTP сервер, middleware, routing, admin API, streaming и non-streaming forwarding. |
| `rotator` | Provider registry, credential rotation, rate limiter, cooldown manager, circuit breaker, model cache, usage accounting. |
| `models` | Shared serde модели для chat, embeddings, files, batches, images, Anthropic messages и admin responses. |
| `proxy_config` | Загрузка конфигурации и env через `dotenvy`/`config`. Путь crate: `crates/config`. |
| `proxy-tui` | Ratatui TUI для мониторинга providers, model cache и usage. Путь crate: `crates/tui`. |

`proxy_app` использует middleware для CORS, gzip compression, tracing и custom logging с redaction чувствительных headers. Запросы проходят через provider registry и rotator, где credentials выбираются через `acquire_least_loaded` с CAS loop. `CredentialPermit` использует RAII для безопасного decrement active usage при завершении запроса.

## Quick start

Запуск HTTP proxy:

```bash
cargo run --bin proxy_app
```

По умолчанию сервер читает `HOST` и `PORT` из env, если они заданы.

Запуск TUI:

```bash
cargo run --bin proxy-tui
```

TUI читает `PROXY_BASE_URL` и `ADMIN_TOKEN`. Если `PROXY_BASE_URL` не задан, используется `http://127.0.0.1:3000`.

## Env vars

| Env var | Назначение |
| --- | --- |
| `OPENAI_API_KEY` | Основной OpenAI-compatible API key. |
| `OPENAI_API_KEY_1` | Дополнительный OpenAI-compatible API key для rotation. Поддерживается pattern с индексами. |
| `ANTHROPIC_API_KEY` | Anthropic API key для `/v1/messages`. |
| `PROVIDER_MODELS` | Provider-specific mapping моделей. Используется provider registry. |
| `MODEL_ALLOWLIST` | Regex allowlist моделей, которые можно отдавать через model endpoints. |
| `MODEL_DENYLIST` | Regex denylist моделей, которые нужно скрыть или запретить. |
| `ADMIN_TOKEN` | Bearer token для admin endpoints и TUI. |

Provider registry поддерживает 20+ providers, включая `openai`, `anthropic`, `gemini`, `nvidia`, `qwen`, `xai` и другие OpenAI-compatible backends.

## API endpoints

| Endpoint | Метод | Назначение |
| --- | --- | --- |
| `/v1/chat/completions` | `POST` | OpenAI-compatible chat completions. Поддерживает streaming и non-streaming responses. |
| `/v1/embeddings` | `POST` | Embeddings forwarding. |
| `/v1/models` | `GET` | Список доступных моделей с provider registry и model filters. |
| `/v1/files` | `GET`, `POST` | Files API. |
| `/v1/batches` | `GET`, `POST` | Batches API. |
| `/v1/images/generations` | `POST` | Image generation API. |
| `/v1/messages` | `POST` | Anthropic Messages API. |
| `/admin/stats` | `GET` | Runtime stats для credentials, providers, usage и cache. Требует `ADMIN_TOKEN`. |
| `/admin/token_count` | `POST` | Token count estimate. Требует `ADMIN_TOKEN`. |
| `/admin/cost_estimate` | `POST` | Cost estimate. Требует `ADMIN_TOKEN`. |

## Provider behavior

- Credential rotation выбирает наименее загруженный key через `acquire_least_loaded` и CAS loop.
- Token bucket rate limiter работает per key.
- Cooldown manager временно исключает проблемные credentials.
- Circuit breaker поддерживает состояния `closed`, `open`, `half-open`.
- Model cache использует TTL и tolerant parser для OpenAI, Gemini и plain array responses.
- Usage accounting пишет данные через background flush в JSON.
- Model filters применяют `MODEL_ALLOWLIST` и `MODEL_DENYLIST`.
- Provider-specific transforms обрабатывают отличия upstream API, включая strip `stream_options` для Anthropic и Gemini model prefix.
- OAuth provider поддерживает async token refresh.

## Testing

Запуск всех тестов workspace:

```bash
cargo test --workspace
```

Проверка warnings через clippy:

```bash
cargo clippy --workspace -- -D warnings
```

Текущее состояние миграции: 129 tests проходят, `cargo clippy --workspace -- -D warnings` чист.

## Миграция из Python

Перенесены основные части Python проекта:

| Python capability | Rust реализация |
| --- | --- |
| FastAPI HTTP server | `proxy_app` на Axum. |
| `rotator_library` | `rotator` crate с rotation, retries, rate limiting, cooldown и circuit breaker. |
| Pydantic models | `models` crate с serde models. |
| `.env` и dataclass config | `proxy_config` crate с env/config loading. |
| Provider definitions | Provider registry с 20+ providers и model metadata. |
| Streaming proxy | SSE streaming для `/v1/chat/completions` и provider forwarding. |
| Usage tracking | JSON usage accounting с background flush. |
| Admin utilities | `/admin/stats`, `/admin/token_count`, `/admin/cost_estimate`. |
| Runtime monitoring | `proxy-tui` на ratatui. |
