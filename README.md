# HTTP Forward Proxy (Rust)

Async forward proxy with HTTP/1.1 absolute-form support and HTTPS tunneling via CONNECT. Built with Tokio + Hyper, structured logging via tracing, and a stub cache interface wired for future implementation.

## Milestones
- M1: HTTP forward proxy (GET/POST) without CONNECT
- M2: CONNECT tunnel support
- M3: Connection pooling (optional)
- M4: Real caching implementation
- M5: Metrics endpoint and exporters

M1 and M2 are fully implemented in this workspace, with tests.

## Workspace Layout
- `crates/proxy/src/main.rs` entrypoint
- `crates/proxy/src/config.rs` config parsing
- `crates/proxy/src/server.rs` listener + routing
- `crates/proxy/src/http_proxy.rs` HTTP forward proxy
- `crates/proxy/src/connect_tunnel.rs` CONNECT tunneling
- `crates/proxy/src/policy.rs` allow/deny policy engine
- `crates/proxy/src/headers.rs` hop-by-hop header stripping
- `crates/proxy/src/cache.rs` cache trait + NoopCache
- `crates/proxy/src/obs.rs` tracing setup
- `crates/proxy/src/errors.rs` error mapping
- `crates/proxy/tests/integration.rs` integration tests
- `config.toml` example config

## Running
- Start the proxy with the default config: `cargo run -p proxy`
- Use a custom config path: `cargo run -p proxy -- ./path/to/config.toml`

## Tests
- Run all tests: `cargo test`

## Notes
- CONNECT tunnels create a TCP pipe; no TLS interception.
- Streaming is end-to-end; bodies are not buffered.
- Hop-by-hop headers and Proxy-Authorization are stripped before forwarding.
- Cache is stubbed as NoopCache even when enabled; wiring is tested for future work.
