use anyhow::{anyhow, Result};
use base64;
use config::Config;
use dashmap::DashMap;
use env_logger;
use futures::stream::SplitStream;
use futures::{stream::SplitSink, SinkExt, StreamExt};
use log::{debug, error, info};
use rand;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::io::{split, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::{timeout, Duration};
use tokio_tungstenite::{accept_async, tungstenite::protocol::Message, WebSocketStream};
use uuid::Uuid;

/// エージェントとクライアント間でやり取りするメッセージのペイロード定義
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", content = "payload")]
enum Payload {
    #[serde(rename = "init-request")]
    InitRequest {
        agent_id: String,
        ip: String,
        remote_host: String,
        country_code: String,
        city: String,
        region: String,
        asn: String,
        asn_org: String,
    },
    #[serde(rename = "init-response")]
    InitResponse {
        success: bool,
        message: Option<String>,
    },
    #[serde(rename = "init-error")]
    InitError { error_message: String },
    #[serde(rename = "connect-request")]
    ConnectRequest {
        request_id: String,
        target_addr: String,
        target_port: u16,
        agent_id: Option<String>,
        address_type: u8,
    },
    #[serde(rename = "connect-response")]
    ConnectResponse { request_id: String, success: bool },
    #[serde(rename = "data-chunk-request")]
    DataRequestChunk {
        request_id: String,
        chunk_id: u32,
        data: String,
    },
    #[serde(rename = "data-chunk-response")]
    DataResponseChunk {
        request_id: String,
        chunk_id: u32,
        data: String,
    },
    #[serde(rename = "data-response-transfer-complete")]
    DataResponseTransferComplete {
        request_id: String,
        success: bool,
        error_message: Option<String>,
    },
    #[serde(rename = "data-request-transfer-complete")]
    DataRequestTransferComplete {
        request_id: String,
        success: bool,
        error_message: Option<String>,
    },
    #[serde(rename = "client-disconnect")]
    ClientDisconnect { request_id: String },
}

// 型エイリアス: WebSocketの送受信部分を分割した型
type WsSink = SplitSink<WebSocketStream<TcpStream>, Message>;
type WsStream = SplitStream<WebSocketStream<TcpStream>>;

/// エージェント接続管理構造体
/// WebSocketの送信側（Sink）をMutexで保護して並行アクセス可能に
struct AgentConnection {
    sink: Mutex<WsSink>,
    metadata: AgentMetadata,
}

/// エージェントのメタデータ構造体
#[derive(Debug, Clone)]
#[allow(dead_code)] // 未使用フィールドの警告を抑制
struct AgentMetadata {
    ip: String,
    remote_host: String,
    country_code: String,
    city: String,
    region: String,
    asn: String,
    asn_org: String,
}

/// エージェント管理用のスレッドセーフなHashMap
/// キー: エージェントID, 値: AgentConnectionのArc
type AgentMap = DashMap<String, Arc<AgentConnection>>;

/// 非同期処理間の通信チャネル管理用列挙型
/// Oneshot: 単発のレスポンス用チャネル
/// Mpsc: ストリームデータ転送用チャネル
enum PendingSender {
    Oneshot(oneshot::Sender<Payload>),
    Mpsc(mpsc::Sender<Payload>),
}

/// リクエストIDと対応するチャネルのマッピング
/// Arc<Mutex<...>> でスレッドセーフな共有を実現
type PendingMap = Arc<Mutex<HashMap<String, PendingSender>>>;

/// WebSocket経由でペイロードを送信するヘルパー関数
/// JSONシリアライズ → Textメッセージとして送信
async fn send_message(sink: &Mutex<WsSink>, payload: Payload) -> Result<()> {
    let json = serde_json::to_string(&payload)?;
    let mut lock = sink.lock().await;
    lock.send(Message::Text(json)).await?;
    Ok(())
}

/// エージェントからの初期化リクエストを処理し、AgentMap に登録して InitResponse を送信する
async fn handle_init_request(
    agent_id: String,
    sink: WsSink,
    agents: Arc<AgentMap>,
    metadata: AgentMetadata,
) -> Result<()> {
    info!(
        "[Init] Received init-request from agent. Agent ID: {} | Metadata: {:?}",
        agent_id, metadata
    );

    // エージェントIDのフォーマットチェックを追加
    if !agent_id.starts_with("agent_") {
        let temp_conn = Arc::new(AgentConnection {
            sink: Mutex::new(sink),
            metadata,
        });
        send_message(
            &temp_conn.sink,
            Payload::InitResponse {
                success: false,
                message: Some("Agent ID must start with 'agent_'".to_string()),
            },
        )
        .await?;
        return Err(anyhow!("Invalid agent ID format: {}", agent_id));
    }

    let agent_conn = Arc::new(AgentConnection {
        sink: Mutex::new(sink),
        metadata,
    });
    agents.insert(agent_id.clone(), agent_conn.clone());
    // 初期化レスポンス送信
    send_message(
        &agent_conn.sink,
        Payload::InitResponse {
            success: true,
            message: None,
        },
    )
    .await?;
    info!(
        "[Init] Agent registered successfully. Agent ID: {}",
        agent_id
    );
    Ok(())
}

/// エージェントから受信した ConnectResponse を、PendingMap 経由で送信元に通知する
async fn handle_connect_response(payload: Payload, pending: PendingMap) -> Result<()> {
    if let Payload::ConnectResponse { request_id, .. } = &payload {
        let mut lock = pending.lock().await;
        if let Some(pending_sender) = lock.remove(request_id) {
            match pending_sender {
                PendingSender::Oneshot(sender) => {
                    if let Err(e) = sender.send(payload) {
                        error!("Failed to send oneshot response: {:?}", e);
                    }
                }
                _ => {
                    error!("Unexpected pending sender type for connect response");
                }
            }
        }
    } else {
        error!(
            "Received unexpected message in handle_connect_response: {:?}",
            payload
        );
    }
    Ok(())
}

/// エージェントからの DataResponseChunk または DataResponseTransferComplete を処理する
async fn handle_data_response(payload: Payload, pending: PendingMap) -> Result<()> {
    match &payload {
        Payload::DataResponseChunk { request_id, .. }
        | Payload::DataResponseTransferComplete { request_id, .. } => {
            let mut lock = pending.lock().await;
            if let Some(pending_sender) = lock.get_mut(request_id) {
                match pending_sender {
                    PendingSender::Mpsc(sender) => {
                        if let Err(e) = sender.send(payload).await {
                            error!("Failed to send data response: {:?}", e);
                        }
                    }
                    _ => {
                        error!("Unexpected pending sender type for data response");
                    }
                }
            } else {
                error!(
                    "No pending sender found for data response with request_id: {}",
                    request_id
                );
            }
        }
        _ => {
            debug!("Received unexpected message: {:?}", payload);
        }
    }
    Ok(())
}

/// 各エージェントとの WebSocket 接続のイベントループ
async fn handle_agent_connection(stream: TcpStream, agents: Arc<AgentMap>, pending: PendingMap) {
    // ハンドシェイク実施
    // let peer_addr = stream.peer_addr().ok();
    if let Err(e) = async {
        let ws_stream = accept_async(stream).await.map_err(|e| {
            error!("[Control] WebSocket handshake failed: {:?}", e);
            e
        })?;
        let (sink, mut stream): (WsSink, WsStream) = ws_stream.split();
        // 初回メッセージとして init-request を待つ
        let init_msg = stream.next().await;
        if init_msg.is_none() {
            error!("WebSocket connection closed before init-request");
            return Ok(());
        }
        let init_msg = init_msg.unwrap();
        let init_payload: Payload = match init_msg {
            Ok(Message::Text(text)) => serde_json::from_str(&text).map_err(|e| {
                error!("Failed to parse JSON from agent: {}", text);
                e
            })?,
            _ => {
                error!("Expected init-request but got non-text message");
                return Ok(());
            }
        };
        let (agent_id, ip, remote_host, country_code, city, region, asn, asn_org) =
            if let Payload::InitRequest {
                agent_id,
                ip,
                remote_host,
                country_code,
                city,
                region,
                asn,
                asn_org,
            } = init_payload
            {
                (
                    agent_id,
                    ip,
                    remote_host,
                    country_code,
                    city,
                    region,
                    asn,
                    asn_org,
                )
            } else {
                error!("Expected init-request but got: {:?}", init_payload);
                return Ok(());
            };

        info!(
            "[Init] Received init-request from agent. Agent ID: {}",
            agent_id
        );
        // エージェント登録＆初期化レスポンス送信
        let metadata = AgentMetadata {
            ip,
            remote_host,
            country_code,
            city,
            region,
            asn,
            asn_org,
        };
        handle_init_request(agent_id.clone(), sink, agents.clone(), metadata).await?;

        // その後のメッセージを処理するループ
        while let Some(message) = stream.next().await {
            match message {
                Ok(Message::Text(text)) => {
                    let payload: Payload = match serde_json::from_str(&text) {
                        Ok(p) => p,
                        Err(e) => {
                            error!("Failed to parse JSON from agent: {}, {:?}", text, e);
                            continue;
                        }
                    };
                    match payload {
                        Payload::ConnectResponse { .. } => {
                            if let Err(e) = handle_connect_response(payload, pending.clone()).await
                            {
                                error!("Error handling connect-response: {:?}", e);
                            }
                        }
                        Payload::DataResponseChunk { .. }
                        | Payload::DataResponseTransferComplete { .. } => {
                            if let Err(e) = handle_data_response(payload, pending.clone()).await {
                                error!("Error handling data-response: {:?}", e);
                            }
                        }
                        _ => {
                            debug!("Received unexpected message from agent: {:?}", payload);
                        }
                    }
                }
                Ok(other) => {
                    error!("Unexpected WebSocket message: {:?}", other);
                }
                Err(e) => {
                    error!("[{}] WebSocket error: {:?}", agent_id, e);
                    break;
                }
            }
        }
        info!("[{}] Connection closed", agent_id);
        // エージェント切断時は AgentMap から削除
        agents.remove(&agent_id);
        Ok::<(), anyhow::Error>(())
    }
    .await
    {
        error!("Error in handle_agent_connection: {:?}", e);
    }
}

/// SOCKS5 ハンドシェイク（認証なし）の実装
async fn perform_socks5_handshake(
    stream: &mut TcpStream,
    client_addr: &SocketAddr,
) -> Result<Option<String>> {
    info!("[SOCKS5] Handshake started with {}", client_addr);
    let mut buf = [0u8; 2];
    stream.read_exact(&mut buf).await?;
    if buf[0] != 0x05 {
        return Err(anyhow!("Unsupported SOCKS version: {}", buf[0]));
    }
    let nmethods = buf[1] as usize;
    let mut methods = vec![0u8; nmethods];
    stream.read_exact(&mut methods).await?;

    // ユーザー名/パスワード認証をサポート
    let use_auth = methods.contains(&0x02);
    let selected_method = if use_auth { 0x02 } else { 0x00 };

    stream.write_all(&[0x05, selected_method]).await?;

    let username = if use_auth {
        // 認証情報の読み取り
        let mut auth_header = [0u8; 2];
        stream.read_exact(&mut auth_header).await?;
        if auth_header[0] != 0x01 {
            return Err(anyhow!("Unsupported auth version: {}", auth_header[0]));
        }
        let username_len = auth_header[1] as usize;
        let mut username = vec![0u8; username_len];
        stream.read_exact(&mut username).await?;
        let username = String::from_utf8(username)?;

        let mut password_len_buf = [0u8; 1];
        stream.read_exact(&mut password_len_buf).await?;
        let password_len = password_len_buf[0] as usize;
        let mut password = vec![0u8; password_len];
        stream.read_exact(&mut password).await?;

        // 認証成功を返答
        stream.write_all(&[0x01, 0x00]).await?;

        Some(username)
    } else {
        None
    };

    Ok(username)
}

/// SOCKS5 の CONNECT リクエストから接続先アドレスとポートを抽出する
async fn read_socks5_connect_request(
    stream: &mut TcpStream,
    client_addr: &SocketAddr,
) -> Result<(u8, String, u16)> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;
    if header[0] != 0x05 {
        return Err(anyhow!(
            "Unsupported SOCKS version in connect request: {}",
            header[0]
        ));
    }
    if header[1] != 0x01 {
        return Err(anyhow!("Unsupported SOCKS command: {}", header[1]));
    }
    let atyp = header[3];
    let target_addr = match atyp {
        0x01 => {
            // IPv4: 4 バイト
            let mut addr_bytes = [0u8; 4];
            stream.read_exact(&mut addr_bytes).await?;
            Ipv4Addr::from(addr_bytes).to_string()
        }
        0x03 => {
            // ドメイン名: 最初の 1 バイトが長さ
            let mut len_buf = [0u8; 1];
            stream.read_exact(&mut len_buf).await?;
            let len = len_buf[0] as usize;
            let mut domain = vec![0u8; len];
            stream.read_exact(&mut domain).await?;
            String::from_utf8(domain)?
        }
        0x04 => {
            // IPv6: 16 バイト（ここでは省略せず文字列化）
            let mut addr_bytes = [0u8; 16];
            stream.read_exact(&mut addr_bytes).await?;
            std::net::Ipv6Addr::from(addr_bytes).to_string()
        }
        _ => return Err(anyhow!("Unsupported address type: {}", atyp)),
    };
    let mut port_buf = [0u8; 2];
    stream.read_exact(&mut port_buf).await?;
    let target_port = u16::from_be_bytes(port_buf);
    info!(
        "[SOCKS5] CONNECT request from {} to {}:{}",
        client_addr, target_addr, target_port
    );
    Ok((atyp, target_addr, target_port))
}

/// 指定のエージェントに connect-request を送信し、タイムアウト付きで connect-response を待つ
async fn send_connect_request_and_wait_for_response(
    agent_id: &str,
    agent_conn: &AgentConnection,
    request_id: &str,
    target_addr: &str,
    target_port: u16,
    address_type: u8,
    pending: PendingMap,
    settings: &Settings,
) -> Result<Payload> {
    let (tx, rx) = oneshot::channel();
    {
        let mut pending_lock = pending.lock().await;
        pending_lock.insert(request_id.to_string(), PendingSender::Oneshot(tx));
    }
    let payload = Payload::ConnectRequest {
        request_id: request_id.to_string(),
        target_addr: target_addr.to_string(),
        target_port,
        agent_id: None,
        address_type,
    };
    info!(
        "[{}] Sent connect-request (Agent: {}, Target: {}:{})",
        request_id, agent_id, target_addr, target_port
    );
    send_message(&agent_conn.sink, payload).await?;
    match timeout(Duration::from_secs(settings.connect_timeout_seconds), rx).await {
        Ok(Ok(response)) => {
            if let Payload::ConnectResponse { success, .. } = &response {
                info!(
                    "[{}] Received connect-response from agent {}. Success: {}",
                    request_id, agent_id, success
                );
            }
            Ok(response)
        }
        Ok(Err(e)) => Err(anyhow!("Oneshot receiver error: {:?}", e)),
        Err(_) => {
            let mut pending_lock = pending.lock().await;
            pending_lock.remove(request_id);
            Err(anyhow!(
                "Timeout waiting for connect response from agent {}",
                agent_id
            ))
        }
    }
}

/// クライアントとエージェント間の双方向データ転送を行う
async fn handle_socks5_data_transfer(
    stream: TcpStream,
    client_addr: SocketAddr,
    request_id: String,
    agent_conn: Arc<AgentConnection>,
    pending: PendingMap,
) -> Result<()> {
    info!("[{}][{}] Data transfer started", request_id, client_addr);
    // TcpStream を split して reader, writer を得る
    let (mut reader, mut writer) = split(stream);
    // エージェントからのデータ応答用 mpsc チャネルを作成し PendingMap に登録
    let (tx, mut rx) = mpsc::channel(32);
    {
        let mut pending_lock = pending.lock().await;
        pending_lock.insert(request_id.clone(), PendingSender::Mpsc(tx));
    }
    // クライアントからのデータ受信タスク（送信タスク）
    let agent_conn_clone = agent_conn.clone();
    let req_id_clone = request_id.clone();
    let client_addr_clone = client_addr;
    let send_task = tokio::spawn(async move {
        let mut chunk_id: u32 = 1;
        let mut buf = [0u8; 1024];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => {
                    info!(
                        "[{}][{}] Client disconnected gracefully",
                        req_id_clone, client_addr_clone
                    );
                    let payload = Payload::ClientDisconnect {
                        request_id: req_id_clone.clone(),
                    };
                    if let Err(e) = send_message(&agent_conn_clone.sink, payload).await {
                        error!(
                            "[{}][{}] Failed to send client disconnect: {:?}",
                            req_id_clone, client_addr_clone, e
                        );
                    }
                    break;
                }
                Ok(n) => {
                    debug!(
                        "[{}][{}] Read {} bytes from client",
                        req_id_clone, client_addr_clone, n
                    );
                    let data_chunk = &buf[..n];
                    let encoded = base64::encode(data_chunk);
                    let payload = Payload::DataRequestChunk {
                        request_id: req_id_clone.clone(),
                        chunk_id,
                        data: encoded,
                    };
                    if let Err(e) = send_message(&agent_conn_clone.sink, payload).await {
                        error!("Failed to send data request: {:?}", e);
                        break;
                    }
                    chunk_id += 1;
                }
                Err(e) => {
                    error!(
                        "[{}][{}] Read error: {}",
                        req_id_clone, client_addr_clone, e
                    );
                    break;
                }
            }
        }
    });
    // エージェントからのデータ受信タスク（書き込みタスク）
    let request_id_clone = request_id.clone();
    let write_task = tokio::spawn(async move {
        while let Some(payload) = rx.recv().await {
            match payload {
                Payload::DataResponseChunk {
                    request_id: _,
                    chunk_id: _,
                    data,
                } => match base64::decode(&data) {
                    Ok(decoded) => {
                        if let Err(e) = writer.write_all(&decoded).await {
                            error!(
                                "[{}][{}] Failed to write data: {:?}",
                                request_id_clone, client_addr_clone, e
                            );
                            break;
                        } else {
                            debug!(
                                "[{}][{}] Wrote {} bytes to client",
                                request_id_clone,
                                client_addr_clone,
                                decoded.len()
                            );
                        }
                    }
                    Err(e) => {
                        error!("Failed to decode base64 data: {:?}", e);
                    }
                },
                Payload::DataResponseTransferComplete {
                    request_id: _,
                    success,
                    error_message,
                } => {
                    if success {
                        info!(
                            "[{}][{}] Data transfer completed successfully",
                            request_id_clone, client_addr_clone
                        );
                    } else {
                        error!(
                            "[{}][{}] Transfer failed: {}",
                            request_id_clone,
                            client_addr_clone,
                            error_message.as_deref().unwrap_or("unknown error")
                        );
                    }
                    info!(
                        "[{}][{}] Data receiver terminated",
                        request_id_clone, client_addr_clone
                    );
                    break;
                }
                _ => {
                    error!("Unexpected payload type in data transfer");
                }
            }
        }
    });
    let _ = tokio::join!(send_task, write_task);
    info!("[{}][{}] Data transfer terminated", request_id, client_addr);
    let request_id_clone = request_id.clone();
    let mut pending_lock = pending.lock().await;
    pending_lock.remove(&request_id_clone);
    Ok(())
}

/// SOCKS5サーバーメイン処理
/// 1. ハンドシェイク処理
/// 2. 接続要求解析
/// 3. 利用可能なエージェントの選択
/// 4. エージェントへの接続要求転送
/// 5. 双方向データ転送の開始
async fn handle_socks5_connection(
    mut stream: TcpStream,
    client_addr: SocketAddr,
    agents: Arc<AgentMap>,
    pending: PendingMap,
    settings: Arc<Settings>,
) {
    info!("[Control] New SOCKS5 connection from {}", client_addr);
    let username = match perform_socks5_handshake(&mut stream, &client_addr).await {
        Ok(u) => u,
        Err(e) => {
            error!("SOCKS5 handshake failed from {}: {:?}", client_addr, e);
            return;
        }
    };

    let (atyp, target_addr, target_port) =
        match read_socks5_connect_request(&mut stream, &client_addr).await {
            Ok(v) => v,
            Err(e) => {
                error!(
                    "Failed to read SOCKS5 request header from {}: {:?}",
                    client_addr, e
                );
                return;
            }
        };

    // エージェント選択ロジック
    let (agent_id, agent_conn) = match username.as_deref() {
        // ケース1: ユーザー名がagent_で始まる場合 → 特定エージェントを直接指定
        Some(agent_id) if agent_id.starts_with("agent_") => {
            // エージェントマップから該当エージェントを検索
            match agents.get(agent_id) {
                Some(entry) => (agent_id.to_string(), entry.value().clone()),
                None => {
                    error!("Specified agent not found: {}", agent_id);
                    let response = [0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
                    let _ = stream.write_all(&response).await;
                    return;
                }
            }
        }

        // ケース2: ユーザー名がallまたは未指定 → 全エージェントからランダム選択
        Some("all") | None => {
            let agents_vec: Vec<_> = agents.iter().collect();
            if agents_vec.is_empty() {
                error!("No available agents");
                let response = [0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
                let _ = stream.write_all(&response).await;
                return;
            }
            // ランダムなインデックスを生成してエージェントを選択
            let index = rand::thread_rng().gen_range(0..agents_vec.len());
            let entry = &agents_vec[index];
            (entry.key().clone(), entry.value().clone())
        }

        // ケース3: ユーザー名がcountry_で始まる場合 → 国コード指定によるフィルタリング
        Some(country) if country.starts_with("country_") => {
            // country_プレフィックスを除去して国コード部分を取得
            let country_part = country.trim_start_matches("country_");

            // 2文字毎に分割して国コードリストを生成（例: "JPUS" → ["JP", "US"]）
            let codes: Vec<&str> = country_part
                .as_bytes()
                .chunks(2)
                .filter_map(|chunk| {
                    if chunk.len() == 2 {
                        std::str::from_utf8(chunk).ok()
                    } else {
                        None // 2文字に満たない部分は無視
                    }
                })
                .collect();

            // 有効な国コードが取得できたか確認
            if codes.is_empty() {
                error!("Invalid country code format: {}", country);
                let response = [0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
                let _ = stream.write_all(&response).await;
                return;
            }

            // 該当国コードのエージェントをフィルタリング
            let agents_vec: Vec<_> = agents
                .iter()
                .filter(|entry| codes.contains(&entry.metadata.country_code.as_str()))
                .collect();

            if agents_vec.is_empty() {
                error!("No agents available in countries: {:?}", codes);
                let response = [0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
                let _ = stream.write_all(&response).await;
                return;
            }

            // フィルタリングされたエージェントからランダム選択
            let index = rand::thread_rng().gen_range(0..agents_vec.len());
            let entry = &agents_vec[index];
            (entry.key().clone(), entry.value().clone())
        }

        // ケース4: 不正なフォーマットの場合
        Some(other) => {
            error!("Invalid username format: {}", other);
            let response = [0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
            let _ = stream.write_all(&response).await;
            return;
        }
    };

    let request_id = Uuid::new_v4().to_string();

    info!(
        "[{}] Selected agent {} for SOCKS5 CONNECT request from {}",
        request_id, agent_id, client_addr
    );
    let connect_response = match send_connect_request_and_wait_for_response(
        &agent_id,
        &agent_conn,
        &request_id,
        &target_addr,
        target_port,
        atyp,
        pending.clone(),
        &settings,
    )
    .await
    {
        Ok(resp) => resp,
        Err(e) => {
            error!(
                "Failed to send connect-request or timed out waiting for response from agent {}: {:?}",
                agent_id, e
            );
            let response = [0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
            let _ = stream.write_all(&response).await;
            return;
        }
    };
    if let Payload::ConnectResponse { success, .. } = connect_response {
        if !success {
            let response = [0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
            if let Err(e) = stream.write_all(&response).await {
                error!(
                    "[{}] Failed to send SOCKS5 CONNECT response to {}: {:?}",
                    request_id, client_addr, e
                );
            } else {
                info!(
                    "[{}] Sent SOCKS5 CONNECT response to {}. Success: false",
                    request_id, client_addr
                );
            }
            return;
        }
    } else {
        error!(
            "Unexpected payload type in response from agent {}",
            agent_id
        );
        let response = [0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
        let _ = stream.write_all(&response).await;
        return;
    }
    // SOCKS5 接続成功応答（IPv4, 0.0.0.0, ポート 0）
    let response = [0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
    if let Err(e) = stream.write_all(&response).await {
        error!(
            "[{}] Failed to send SOCKS5 CONNECT response to {}: {:?}",
            request_id, client_addr, e
        );
        return;
    }
    info!(
        "[{}] Sent SOCKS5 CONNECT response to {}. Success: true",
        request_id, client_addr
    );
    // 双方向のデータ転送を開始
    if let Err(e) =
        handle_socks5_data_transfer(stream, client_addr, request_id, agent_conn, pending.clone())
            .await
    {
        error!("SOCKS5 data transfer error: {:?}", e);
    }
}

/// WebSocket サーバーを起動し、エージェントからの接続を待ち受ける
async fn run_websocket_server(
    agents: Arc<AgentMap>,
    pending: PendingMap,
    settings: Arc<Settings>,
) -> Result<()> {
    let addr = format!("{}:{}", settings.bind_address, settings.websocket_port);
    let listener = TcpListener::bind(&addr).await?;
    info!("[Control] WebSocket server started on {}", addr);
    loop {
        let (stream, addr) = listener.accept().await?;
        info!("[Control] New WebSocket connection from {}", addr);
        let agents_clone = agents.clone();
        let pending_clone = pending.clone();
        let _settings_clone = settings.clone();
        tokio::spawn(async move {
            handle_agent_connection(stream, agents_clone, pending_clone).await;
        });
    }
}

/// SOCKS5 サーバーを起動し、クライアントからの接続を待ち受ける
async fn run_socks5_server(
    agents: Arc<AgentMap>,
    pending: PendingMap,
    settings: Arc<Settings>,
) -> Result<()> {
    let addr = format!("{}:{}", settings.bind_address, settings.socks5_port);
    let listener = TcpListener::bind(&addr).await?;
    info!("[Control] SOCKS5 server started on {}", addr);
    loop {
        let (stream, client_addr) = listener.accept().await?;
        info!("[Control] New SOCKS5 connection from {}", client_addr);
        let agents_clone = agents.clone();
        let pending_clone = pending.clone();
        let settings_clone = settings.clone();
        tokio::spawn(async move {
            handle_socks5_connection(
                stream,
                client_addr,
                agents_clone,
                pending_clone,
                settings_clone,
            )
            .await;
        });
    }
}

#[derive(Debug, Deserialize, Clone)]
struct Settings {
    websocket_port: u16,
    socks5_port: u16,
    bind_address: String,
    connect_timeout_seconds: u64,
}

async fn load_config() -> Result<Settings> {
    let config = Config::builder()
        .add_source(config::File::with_name("config"))
        .build()?;

    config
        .try_deserialize()
        .map_err(|e| anyhow!("Failed to parse config: {}", e))
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // AgentMap と PendingMap を初期化（Arc 経由で各サーバータスクに共有）
    let agents: Arc<AgentMap> = Arc::new(DashMap::new());
    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

    let settings = Arc::new(load_config().await?);

    // WebSocket サーバーと SOCKS5 サーバーを並行して実行
    tokio::select! {
        res = run_websocket_server(agents.clone(), pending.clone(), settings.clone()) => {
            if let Err(e) = res {
                error!("WebSocket server error: {:?}", e);
            }
        },
        res = run_socks5_server(agents.clone(), pending.clone(), settings.clone()) => {
            if let Err(e) = res {
                error!("SOCKS5 server error: {:?}", e);
            }
        }
    }
    Ok(())
}
