<div align="center">
  <h1>Chilsonite</h1>
  <p>A minimal Rotating Proxy written in Rust</p>
</div>

## 概要

Chilsonite は、Rust で書かれた最小限のローテーションプロキシです。

## 特徴

- Rust で書かれており、高速で安全
- websocket を用いてトンネリングするため、ファイアウォールを回避可能
- カントリーコードを指定したプロキシの選択も可能
- ローテーションプロキシとして利用せず、プロキシサーバを指定して利用することも可能

## インストール

[Releases](https://github.com/calloc134/chilsonite/releases) から、各 OS 向けの実行ファイルをダウンロードしてください。
インストールは不要です。

## 使い方

### CICADA - Rotating Proxy Server

CICADA は、マスターとなるプロキシサーバです。
CICADA では、設定について`config.toml`を用いて管理します。

```toml
bind_address = "::"
websocket_port = 3005
socks5_port = 3006
connect_timeout_seconds = 10
```

実行時のカレントディレクトリに`config.toml`を配置し、実行してください。

```bash
$ ./cicada
```

### GARADAMA - Rotating Proxy Client

GARADAMA は、クライアントとなるプロキシサーバです。
GARADAMA にはパラメータがハードコーディングされているため、もし自前の環境で利用する場合は定数の値を変更する必要があります。

```rust
// 接続先URL
const DEFAULT_MASTER_URL: &str = "ws://127.0.0.1:3005";
```

もしくは、実行時のコマンドライン引数で指定することも可能です。

```bash
$ ./garadama (WebSocket URL)
```

## ローテーションプロキシとは？

ローテーションプロキシは、通常、多数の IP アドレスのプールを持っており、リクエストが来るたびに、そのプールから未使用の IP アドレスをランダムに、または特定のロジックに従って選択し、クライアントに割り当てます。  
これにより、クライアントは毎回異なる IP アドレスからアクセスしているように見えます。
これにより、IP ブロッキング・レート制限の回避が可能となり、スクレイピングやデータ収集などの用途で利用されます。

## 仕組み

この回転プロキシでは、2 つのプログラムが存在しています。

### CICADA - Rotating Proxy Server

CICADA は、マスターとなるプロキシサーバです。

複数のプロキシクライアントと WebSocket を用いて通信します。また、回転プロキシを利用するユーザからの Socks5 プロキシリクエストを受け付け、プロキシクライアントにリクエストを転送します。
ユーザからの Socks5 プロキシリクエストでは、利用する GARADAMA クライアントやカントリーコードを指定することができます。名前解決は GARADAMA クライアント側で行います。

### GARADAMA - Rotating Proxy Client

GARADAMA は、クライアントとなるプロキシサーバです。

CICADA と WebSocket を用いて通信します。また、Socks5 プロキシリクエストを受け付け、リクエストを転送します。名前解決の必要があれば GARADAMA が行います。

## 依存関係

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

## ロードマップ

- [x] CICD の実装
- [ ] ファイル構造の分離
- [ ] deprecated なクレートの更新
- [ ] Android、Raspberry Pi への対応
- [ ] テストの追加

## ライセンス

MIT

## 作者

- [calloc134](https://github.com/calloc134)

## 余談

名前の由来はわかりますか？笑
