# HTTP Proxy Cache Roadmap

This roadmap reflects the current state of the codebase as audited on March 8, 2026. Checked items are implemented in the repository today. Unchecked items are missing, partial, or not wired through fully enough to count as done.

## Product Shape

- [x] HTTP/1.1 forward proxy behavior for absolute-form requests
- [x] Origin-form rewrite before upstream forwarding
- [x] HTTPS tunneling through `CONNECT`
- [x] Domain and port allow/deny policy enforcement
- [ ] Reverse proxy behavior with configured upstream backends
- [ ] Host/path based routing for reverse proxy use cases
- [ ] TLS termination for inbound HTTPS traffic
- [ ] HTTP/2 support on inbound proxy connections
- [ ] HTTP/2 or HTTP/3 support to upstream services

## Core Request Handling

- [x] Listener startup from TOML configuration
- [x] Concurrency limiting through a connection semaphore
- [x] Request-scoped tracing spans with request IDs
- [x] End-to-end streaming response bodies
- [x] Hop-by-hop header stripping before forwarding
- [x] `Proxy-Authorization` stripping before forwarding
- [x] CONNECT tunnel idle timeout handling
- [x] Upstream connect timeout handling
- [x] Real per-IP rate limiting enforcement
- [x] True header byte-size limit enforcement
- [ ] Graceful shutdown flow for listener and in-flight requests
- [ ] Request body size limits
- [ ] Circuit breaking or overload protection beyond max connections

## Caching Foundation

- [x] Cache backend abstraction
- [x] No-op cache backend when caching is disabled
- [x] SQLite-backed metadata store
- [x] On-disk cached object storage under `.cache/objects`
- [x] Cache entry lookup by method and URL
- [x] Cache insert and overwrite behavior
- [x] Cache delete behavior
- [x] Automatic removal of expired cache entries on lookup
- [x] Automatic removal of entries whose body file is missing
- [x] LRU-style eviction based on `last_access`
- [x] Max entry count enforcement
- [x] GET cache reads
- [x] HEAD cache reads
- [x] GET cache writes
- [x] HEAD metadata-only cache writes
- [x] Cache bypass for range requests
- [x] Cache bypass for `206 Partial Content` responses
- [x] Cache bypass for oversized responses
- [x] Cache bypass when `Content-Length` is required but missing
- [x] Cache write deduplication per cache key
- [x] Streaming tee from upstream response body into cache writer
- [x] Temporary-file write then atomic rename on successful cache commit
- [x] Cache discard on upstream body error
- [x] Cache discard on content-length mismatch
- [x] Cache discard on streaming backpressure / full cache buffer
- [ ] Cache persistence by default in development mode
- [ ] Configurable cache root directory instead of hardcoded `.cache`
- [ ] Background cache cleanup independent of reads/writes

## HTTP Cache Semantics

- [x] Local default TTL for cache entries
- [x] `Cache-Control: no-store` handling
- [x] `Cache-Control: no-cache` handling as non-cacheable
- [x] `Cache-Control: max-age` handling
- [x] `Expires` header handling
- [x] Only cache successful upstream responses
- [ ] `ETag` based conditional revalidation
- [ ] `Last-Modified` / `If-Modified-Since` revalidation
- [ ] `Vary` header support in cache key selection
- [ ] `private` / `public` cache directives
- [ ] `must-revalidate` handling
- [ ] `stale-while-revalidate` handling
- [ ] `stale-if-error` handling
- [ ] Age calculation and `Age` header emission
- [ ] Range-aware caching and partial object assembly
- [ ] Cache key normalization beyond raw `method + URL`

## HTTPS Asset Caching

- [ ] Decide the HTTPS caching model: reverse proxy TLS termination vs forward-proxy TLS interception
- [ ] Reverse-proxy TLS termination path for cacheable HTTPS assets
- [ ] Forward-proxy MITM/TLS interception path for cacheable HTTPS assets
- [ ] Certificate and private key management for terminated HTTPS traffic
- [ ] Internal CA generation and trust workflow if forward-proxy interception is chosen
- [ ] SNI-based routing for terminated HTTPS traffic
- [ ] Decrypted HTTP request/response handling after TLS termination
- [ ] Reuse the existing cache pipeline for decrypted HTTPS responses
- [ ] Cache policy and keying rules for HTTPS assets
- [ ] Security model for sensitive HTTPS responses to avoid caching private data
- [ ] Operator controls to scope which hosts or paths are eligible for HTTPS caching
- [ ] Tests proving HTTPS assets can be cached and replayed without CONNECT pass-through
- [ ] Documentation covering the security and operational implications of HTTPS caching

## Upstream Connectivity

- [x] Direct TCP connect to upstream origin
- [x] Per-request upstream HTTP/1.1 client handshake
- [ ] Upstream connection pooling
- [ ] Keep-alive reuse across requests
- [ ] DNS caching
- [ ] Retry policy for safe idempotent requests
- [ ] Proxy chaining to another upstream proxy
- [ ] Health checks for configured reverse-proxy backends

## Observability

- [x] Tracing subscriber initialization
- [x] Pretty log output mode
- [x] JSON log output mode
- [x] Configurable log level
- [x] Configurable log timezone rendering
- [x] Per-request timing logs including duration and TTFB
- [x] Cache hit/miss/store logging
- [x] Tunnel lifecycle logging
- [x] Honor `logging.request_id` as a real feature toggle
- [x] Honor `logging.redact_headers` when request/response headers are logged
- [ ] Metrics endpoint
- [ ] Prometheus or OpenTelemetry exporters
- [ ] Structured counters for cache hits, misses, bypasses, evictions, and errors

## Configuration and UX

- [x] TOML configuration loading
- [x] Reasonable default configuration values
- [x] Configurable listen host and port
- [x] Configurable cache enablement and TTL
- [x] Configurable cache entry limit
- [x] Configurable cache writer channel size
- [x] Configurable max object size
- [x] Configurable content-length requirement for caching
- [x] Configurable cache writer slowdown for testing
- [x] Configurable policy allow/deny lists
- [x] Configuration validation with startup-time warnings/errors for invalid combinations
- [x] Separate dev/test/prod example configs
- [ ] Reverse-proxy-specific config model

## Testing

- [x] Unit tests for header stripping
- [x] Unit tests for cache-control parsing
- [x] Unit tests for domain wildcard matching
- [x] Unit tests for absolute-form URI rewriting
- [x] Integration test for forward proxy request forwarding
- [x] Integration test for CONNECT tunnel byte flow
- [x] Integration test for cache stream and replay behavior
- [x] Integration test for cache discard on upstream body failure
- [x] Integration test for range response bypass
- [x] Integration test for cache disablement under buffer pressure
- [x] Integration test for HEAD cache replay
- [x] Integration test for cache expiry behavior
- [x] Integration test for LRU eviction behavior
- [x] Integration test for missing body file invalidation
- [x] Integration test for policy denial paths
- [x] Integration test for startup behavior in dev vs prod cache modes
- [ ] Load tests and benchmark coverage

## Comprehensive Test Coverage

- [ ] Define a coverage target for all core modules and enforce it in CI
- [ ] Add unit tests for every public function and critical internal helper
- [x] Add subsystem tests for configuration parsing and validation
- [ ] Add subsystem tests for tracing and logging behavior
- [x] Add subsystem tests for policy allow and deny edge cases
- [x] Add subsystem tests for request rewriting and header sanitation edge cases
- [x] Add subsystem tests for cache metadata storage and retrieval
- [x] Add subsystem tests for cache object file lifecycle
- [x] Add subsystem tests for cache eviction and invalidation behavior
- [ ] Add subsystem tests for timeout handling and connection lifecycle management
- [x] Add integration tests for every supported HTTP method and unsupported method handling
- [x] Add integration tests for all cache hit, miss, bypass, expire, and eviction paths
- [ ] Add integration tests for concurrent requests against the same cache key
- [ ] Add integration tests for concurrent requests across many cache keys
- [x] Add integration tests for error responses, malformed requests, and upstream failures
- [x] Add integration tests for policy-denied hosts and ports
- [ ] Add integration tests for large payloads, slow upstreams, and slow clients
- [ ] Add integration tests for shutdown and restart behavior
- [ ] Add integration tests for dev-mode logging and cache reset behavior
- [ ] Add integration tests for prod-mode persistence behavior
- [ ] Add end-to-end tests for reverse-proxy flows if reverse proxy support is added
- [ ] Add end-to-end tests for HTTPS asset caching if HTTPS termination/interception is added
- [ ] Add regression tests for every production bug fixed going forward
- [ ] Add fault-injection tests for disk errors, SQLite errors, and partial body streams
- [ ] Add fuzz tests for HTTP parsing, header handling, and config parsing
- [ ] Add stress tests for connection limits and cache backpressure behavior
- [ ] Add performance benchmarks for proxy throughput, tunnel throughput, and cache hit latency
- [ ] Add long-running soak tests for memory growth, file descriptor leaks, and cache stability

## Documentation Alignment

- [x] README correctly identifies the project as a forward proxy
- [x] README documents CONNECT support
- [x] README documents SQLite-backed caching at a high level
- [x] README updated to reflect that real caching is now implemented beyond the milestone text
- [x] README updated to document current caching limitations and non-goals
- [ ] Documentation for operating this as a reverse proxy, if reverse proxy support is added

## Recommended Next Execution Order

- [x] Enforce real rate limiting and correct header-size limits
- [x] Wire logging config fully, especially header redaction
- [x] Add cache expiry and eviction integration tests
- [x] Add HEAD cache replay coverage
- [ ] Implement upstream connection pooling
- [ ] Decide whether this project remains a forward proxy or should be redesigned as a reverse proxy
- [ ] If reverse proxy is the goal, introduce a new routing/config model instead of patching the current forward-proxy path
