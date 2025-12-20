# HTTP Forward Proxy (Rust)

Async forward proxy with HTTP/1.1 absolute-form support and HTTPS tunneling via CONNECT. Built with Tokio + Hyper, structured logging via tracing, and SQLite-backed caching.

## Milestones
- M1: HTTP forward proxy (GET/POST) without CONNECT
- M2: CONNECT tunnel support
- M3: Connection pooling (optional)
- M4: Real caching implementation
- M5: Metrics endpoint and exporters

M1 and M2 are fully implemented in this workspace, with tests.

## Layout
- `src/main.rs` entrypoint
- `src/config.rs` config parsing
- `src/server.rs` listener + routing
- `src/http_proxy.rs` HTTP forward proxy
- `src/connect_tunnel.rs` CONNECT tunneling
- `src/policy.rs` allow/deny policy engine
- `src/headers.rs` hop-by-hop header stripping
- `src/cache.rs` cache trait + SQLite + NoopCache
- `src/obs.rs` tracing setup
- `src/errors.rs` error mapping
- `tests/integration.rs` integration tests
- `config.toml` example config

## Running
- Start the proxy with the default config: `cargo run`
- Use a custom config path: `cargo run -- ./path/to/config.toml`

## Tests
- Run all tests: `cargo test`

## Notes
- CONNECT tunnels create a TCP pipe; no TLS interception.
- Streaming is end-to-end; bodies are not buffered.
- Hop-by-hop headers and Proxy-Authorization are stripped before forwarding.
- Cache uses SQLite metadata in `.cache/cache.sqlite` with LRU eviction, and stores bodies under `.cache/objects/`.
