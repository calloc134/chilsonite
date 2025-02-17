<div align="center">
  <h1>Chilsonite</h1>
  <p>A minimal Rotating Proxy written in Rust</p>
    <a href="./README.ja.md">Link to Japanese version</a>
</div>

## Overview

Chilsonite is a minimal rotating proxy written in Rust.

## Features

- Fast and secure Rust implementation
- WebSocket tunneling for firewall bypass
- Country-based proxy selection
- Can be used as a rotating proxy or with a specified proxy server

## Installation

Please download the executable file from [Releases](https://github.com/calloc134/chilsonite/releases) for each OS.
No installation is required.

## Usage

### CICADA - Rotating Proxy Server

CICADA is the master proxy server.
CICADA manages settings using `config.toml`.

```toml
bind_address = "::"
websocket_port = 3005
socks5_port = 3006
connect_timeout_seconds = 10
```

Place `config.toml` in the current directory when executing, and then run it.

```bash
$ ./cicada
```

### GARADAMA - Rotating Proxy Client

GARADAMA is the client-side proxy server.
Parameters are hardcoded in GARADAMA, so if you are using it in your own environment, you need to change the constant values.

```rust
// Connection URL
const DEFAULT_MASTER_URL: &str = "ws://127.0.0.1:3005";
```

Alternatively, you can specify it as a command-line argument when executing.

```bash
$ ./garadama (WebSocket URL)
```

## What is a Rotating Proxy?

A rotating proxy typically has a pool of many IP addresses. For each incoming request, it selects an unused IP address from the pool, either randomly or according to a specific logic, and assigns it to the client. This makes it appear as if the client is accessing from a different IP address each time.

This allows bypassing IP blocking and rate limits, and is used for applications such as scraping and data collection.

## How it Works

This rotating proxy consists of two programs.

### CICADA - Rotating Proxy Server

CICADA is the master proxy server.

It communicates with multiple proxy clients using WebSockets. It also accepts SOCKS5 proxy requests from users who want to use the rotating proxy, and forwards the requests to the proxy clients.

In SOCKS5 proxy requests from users, you can specify the GARADAMA client and country code to use. Name resolution is performed on the GARADAMA client side.

### GARADAMA - Rotating Proxy Client

GARADAMA is the client-side proxy server.

It communicates with CICADA using WebSockets. It also accepts SOCKS5 proxy requests and forwards them. If name resolution is required, GARADAMA handles it.

## Dependencies

| Crate               | Version | Features |
| ------------------- | ------- | -------- |
| `tokio`             | 1       | `full`   |
| `tokio-tungstenite` | 0.20.1  |          |
| `base64`            | 0.21.5  |          |
| `serde`             | 1.0     | `derive` |
| `serde_json`        | 1.0     |          |
| `futures`           | 0.3.31  |          |
| `anyhow`            | 1.0.95  |          |
| `uuid`              | 1       | `v4`     |
| `rand`              | 0.9.0   |          |
| `log`               | 0.4.25  |          |
| `env_logger`        | 0.11.6  |          |
| `url`               | 2.5.4   |          |
| `ureq`              | 3.0.5   | `json`   |
| `machine-uid`       | 0.5.3   |          |
| `dashmap`           | 6.1.0   |          |
| `config`            | 0.15.8  |          |

## Roadmap

- [x] CI/CD implementation
- [ ] Separation of file
- [ ] Update deprecated crates
- [ ] Support for Android and Raspberry Pi
- [ ] Add tests

## License

MIT

## Author

- [calloc134](https://github.com/calloc134)

## Trivia

Do you know where the name comes from? 😉
