// agent.rs
use anyhow::Result;
use base64;
use common::Payload;
use env_logger;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use log::{debug, error, info};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::{
    connect_async, tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream,
};
use ureq::{self, Agent};
use url::Url;

// 接続先URL
const DEFAULT_MASTER_URL: &str = "ws://127.0.0.1:3005";

// 型エイリアス: WebSocketの送受信部分を分割した型
type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;
type WsStream = SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;

// グローバルなTCP接続マップ（接続確立時はTCPストリームを分割して書き込み側を保持）
type ConnectionMap = Arc<Mutex<HashMap<String, WriteHalf<TcpStream>>>>;

/// WebSocket送信部分へペイロードをJSON化して送信する
async fn send_message(sink: Arc<Mutex<WsSink>>, payload: Payload) -> Result<()> {
    let json = serde_json::to_string(&payload)?;
    let mut guard = sink.lock().await;
    guard.send(Message::Text(json)).await?;
    Ok(())
}

/// エージェント起動時に初期化リクエストを送信する
async fn handle_init_request(agent_id: &str, sink: Arc<Mutex<WsSink>>) -> Result<()> {
    info!("[Init] Determining geo data...");

    let config = Agent::config_builder()
        .ip_family(ureq::config::IpFamily::Ipv4Only)
        .build();

    let agent: Agent = config.into();

    let geo_data = match agent
        .get("https://ifconfig.co/json")
        .call()?
        .body_mut()
        .read_json::<GeoData>()
    {
        Ok(data) => data,
        Err(e) => return Err(anyhow::anyhow!("JSON parse failed: {}", e)),
    };

    // geo dataを表示
    info!("[Init] IP: {}", geo_data.ip);
    info!("[Init] Country Code: {}", geo_data.country_iso);
    info!("[Init] Region: {}", geo_data.region_name);
    info!("[Init] City: {}", geo_data.city);
    info!("[Init] ASN: {}", geo_data.asn);
    info!("[Init] ASN Org: {}", geo_data.asn_org);

    let payload = Payload::InitRequest {
        agent_id: agent_id.to_string(),
        ip: geo_data.ip,
        remote_host: geo_data.region_name.clone(),
        country_code: geo_data.country_iso,
        city: geo_data.city,
        region: geo_data.region_name.clone(),
        asn: geo_data.asn,
        asn_org: geo_data.asn_org,
    };

    send_message(sink.clone(), payload).await?;
    info!("[{}] Sent init-request to master", agent_id);
    Ok(())
}

/// TCP接続の読み取り側タスク（読み出したデータをチャンクに分割してWebSocketで送信）
async fn tcp_read_handler(
    request_id: String,
    mut read_half: ReadHalf<TcpStream>,
    sink: Arc<Mutex<WsSink>>,
) -> Result<()> {
    let mut buffer = [0u8; 1024];
    let mut chunk_id = 1;
    loop {
        match read_half.read(&mut buffer).await {
            Ok(0) => {
                info!(
                    "[{}] TCP read handler terminated (connection closed)",
                    request_id
                );
                let payload = Payload::DataResponseTransferComplete {
                    request_id: request_id.clone(),
                    success: true,
                    error_message: None,
                };
                send_message(sink.clone(), payload).await?;
                info!(
                    "[{}] Data response transfer completed successfully",
                    request_id
                );
                break;
            }
            Ok(n) => {
                debug!("[{}] Read {} bytes from TCP connection", request_id, n);
                let chunk = &buffer[..n];
                let encoded = base64::encode(chunk);
                let payload = Payload::DataResponseChunk {
                    request_id: request_id.clone(),
                    chunk_id,
                    data: encoded,
                };
                send_message(sink.clone(), payload).await?;
                chunk_id += 1;
            }
            Err(e) => {
                error!("[{}] ERROR: Read failed - {}", request_id, e);
                let payload = Payload::DataResponseTransferComplete {
                    request_id: request_id.clone(),
                    success: false,
                    error_message: Some(e.to_string()),
                };
                send_message(sink.clone(), payload).await?;
                info!("[{}] Data response transfer failed: {}", request_id, e);
                break;
            }
        }
    }
    Ok(())
}

/// マスターからの接続要求を受け、指定先へTCP接続を試行する
async fn handle_connect_request(
    request_id: &str,
    target_addr: &str,
    target_port: u16,
    address_type: u8,
    connections: ConnectionMap,
    sink: Arc<Mutex<WsSink>>,
) -> Result<()> {
    info!(
        "[{}] Received connect-request for {}:{} (type: {})",
        request_id, target_addr, target_port, address_type
    );

    // ドメイン名解決が必要な場合
    let resolved_addr = match address_type {
        0x03 => {
            // ドメイン名タイプ
            info!("[{}] Resolving domain: {}", request_id, target_addr);
            match tokio::net::lookup_host(format!("{}:{}", target_addr, target_port)).await {
                Ok(addrs) => {
                    // すべての解決済みアドレスを収集
                    let addresses: Vec<_> = addrs.collect();

                    // 最初のIPv4アドレスを探す
                    let selected_addr = addresses
                        .iter()
                        .find(|a| a.ip().is_ipv4())
                        .or_else(|| addresses.first());

                    match selected_addr {
                        Some(addr) => {
                            let ip_type = if addr.ip().is_ipv4() { "IPv4" } else { "IPv6" };
                            info!(
                                "[{}] DNS resolved to: {} ({})",
                                request_id,
                                addr.ip(),
                                ip_type
                            );
                            addr.ip().to_string()
                        }
                        None => {
                            let msg = format!("DNS resolution failed for {}", target_addr);
                            error!("[{}] {}", request_id, msg);
                            let payload = Payload::ConnectResponse {
                                request_id: request_id.to_string(),
                                success: false,
                            };
                            send_message(sink, payload).await?;
                            return Ok(());
                        }
                    }
                }
                Err(e) => {
                    let msg = format!("DNS resolution error: {}", e);
                    error!("[{}] {}", request_id, msg);
                    let payload = Payload::ConnectResponse {
                        request_id: request_id.to_string(),
                        success: false,
                    };
                    send_message(sink, payload).await?;
                    return Ok(());
                }
            }
        }
        0x01 => {
            // IPv4アドレスの場合
            info!("[{}] Using IPv4 address: {}", request_id, target_addr);
            target_addr.to_string()
        }
        0x04 => {
            // IPv6アドレスの場合
            info!("[{}] Using IPv6 address: {}", request_id, target_addr);
            format!("[{}]", target_addr) // IPv6アドレスを[]で囲む
        }
        _ => {
            error!(
                "[{}] ERROR: Invalid address type: {}",
                request_id, address_type
            );
            let payload = Payload::ConnectResponse {
                request_id: request_id.to_string(),
                success: false,
            };
            send_message(sink, payload).await?;
            return Ok(());
        }
    };

    let addr = format!("{}:{}", resolved_addr, target_port);
    match TcpStream::connect(addr).await {
        Ok(stream) => {
            info!(
                "[{}] Established TCP connection to {}:{}",
                request_id, resolved_addr, target_port
            );
            // TCPストリームをsplitしwrite_halfを保持、read_halfは別タスクで処理
            let (read_half, write_half) = tokio::io::split(stream);
            {
                let mut map = connections.lock().await;
                map.insert(request_id.to_string(), write_half);
            }
            // TCP読み取りタスクをspawn
            let sink_clone = sink.clone();
            let req_id_clone = request_id.to_string();
            tokio::spawn(async move {
                if let Err(e) = tcp_read_handler(req_id_clone.clone(), read_half, sink_clone).await
                {
                    error!("[{}] TCP read handler error: {}", req_id_clone, e);
                }
            });
            // 成功レスポンスを送信
            let payload = Payload::ConnectResponse {
                request_id: request_id.to_string(),
                success: true,
            };
            info!("[{}] Sent connect-response (success: true)", request_id);
            info!("[{}] Data transfer started", request_id);
            send_message(sink, payload).await?;
        }
        Err(e) => {
            error!(
                "[{}] ERROR: Failed to connect to {}:{} - {}",
                request_id, resolved_addr, target_port, e
            );
            info!("[{}] Sent connect-response (success: false)", request_id);
            let payload = Payload::ConnectResponse {
                request_id: request_id.to_string(),
                success: false,
            };
            send_message(sink, payload).await?;
        }
    }
    Ok(())
}

/// マスターから送られてくるデータチャンク要求を処理しTCP接続へ書き込む
async fn handle_data_request(
    request_id: &str,
    chunk_id: u32,
    data: &str,
    connections: ConnectionMap,
) -> Result<()> {
    debug!(
        "[{}] Received data-request (chunk_id: {}, size: {} bytes)",
        request_id,
        chunk_id,
        data.len()
    );
    // Base64デコード
    let decoded = match base64::decode(data) {
        Ok(d) => d,
        Err(e) => {
            error!("[{}] ERROR: Failed to decode base64 - {}", request_id, e);
            return Ok(());
        }
    };
    // 対応するTCP接続のwrite_halfを取得して書き込み
    let mut conns = connections.lock().await;
    if let Some(write_half) = conns.get_mut(request_id) {
        if let Err(e) = write_half.write_all(&decoded).await {
            error!("Failed to write data to TCP connection: {:?}", e);
        } else {
            debug!(
                "[{}] Wrote {} bytes to TCP connection",
                request_id,
                decoded.len()
            );
        }
    } else {
        error!("[{}] ERROR: No TCP connection found", request_id);
    }
    Ok(())
}

/// マスターからの初期化レスポンスを処理してログ出力する
fn handle_init_response(success: bool, message: Option<String>) -> Result<()> {
    if success {
        info!("[Init] Initialization succeeded");
        if let Some(msg) = message {
            info!("[Init] Server message: {}", msg);
        }
    } else {
        error!("[Init] ERROR: Initialization failed");
        if let Some(msg) = message {
            error!("[Init] Server error: {}", msg);
        }
    }
    Ok(())
}

/// WebSocketの受信ループ（イベントループ）: マスターからのメッセージをハンドリング
/// マスターから受信したメッセージに応じて各ハンドラをspawnする
async fn event_loop(
    mut stream: WsStream,
    sink: Arc<Mutex<WsSink>>,
    connections: ConnectionMap,
) -> Result<()> {
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                // JSONパース
                match serde_json::from_str::<Payload>(&text) {
                    Ok(payload) => match payload {
                        Payload::InitResponse { success, message } => {
                            info!("[Control] Received init-response");
                            let _ = handle_init_response(success, message);
                        }
                        Payload::ConnectRequest {
                            request_id,
                            target_addr,
                            target_port,
                            address_type,
                            ..
                        } => {
                            info!("[Control] Received connect-request");
                            let sink_clone = sink.clone();
                            let connections_clone = connections.clone();
                            let req_id = request_id.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_connect_request(
                                    &req_id,
                                    &target_addr,
                                    target_port,
                                    address_type,
                                    connections_clone,
                                    sink_clone,
                                )
                                .await
                                {
                                    error!("[{}] Error handling connect-request: {}", req_id, e);
                                }
                            });
                        }
                        Payload::DataRequestChunk {
                            request_id,
                            chunk_id,
                            data,
                        } => {
                            debug!("[Control] Received data-chunk-request");
                            let connections_clone = connections.clone();
                            let req_id = request_id.clone();
                            tokio::spawn(async move {
                                if let Err(e) =
                                    handle_data_request(&req_id, chunk_id, &data, connections_clone)
                                        .await
                                {
                                    error!("[{}] Error handling data-chunk-request: {}", req_id, e);
                                }
                            });
                        }
                        Payload::DataRequestTransferComplete {
                            request_id,
                            success,
                            error_message,
                        } => {
                            info!("[Control] Received data-request-transfer-complete");
                            let mut conns = connections.lock().await;
                            if let Some(mut write_half) = conns.remove(&request_id) {
                                if !success {
                                    error!(
                                        "[{}] Transfer failed: {}",
                                        request_id,
                                        error_message.unwrap_or_default()
                                    );
                                }

                                if let Err(e) = write_half.shutdown().await {
                                    error!(
                                        "[{}] Error shutting down connection: {}",
                                        request_id, e
                                    );
                                }
                                info!(
                                    "[{}] Connection closed after transfer complete (success: {})",
                                    request_id, success
                                );
                                info!("[{}] Data request transfer completed", request_id,);
                            }
                        }
                        Payload::ClientDisconnect { request_id } => {
                            info!("[Control] Received client-disconnect for {}", request_id);
                            let mut conns = connections.lock().await;
                            if let Some(mut write_half) = conns.remove(&request_id) {
                                if let Err(e) = write_half.shutdown().await {
                                    error!(
                                        "[{}] Error shutting down connection: {}",
                                        request_id, e
                                    );
                                }
                                info!("[{}] Connection closed and removed", request_id);
                            } else {
                                info!("[{}] No active connection found", request_id);
                            }
                        }
                        _ => {
                            debug!("[Control] Received unhandled message type");
                        }
                    },
                    Err(e) => {
                        error!(
                            "[Control] ERROR: Failed to parse message: {}, {:?}",
                            text, e
                        );
                    }
                }
            }
            Ok(_) => {
                debug!("[Control] Received non-text message");
            }
            Err(e) => {
                error!("[Control] ERROR: WebSocket error - {}", e);
                break;
            }
        }
    }
    info!("[Control] Disconnected from master program");
    Ok(())
}

/// メインエントリーポイント: エージェントの起動とマスターサーバーへの接続
#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // マシンIDを取得
    let agent_id = machine_uid::get()
        .expect("Failed to get machine ID")
        .replace("-", ""); // 必要に応じてハイフンを除去
    let agent_id = format!("agent_{}", &agent_id[..12]); // 先頭12文字を使用

    // コマンドライン引数からマスターのWebSocket URLを取得（無ければデフォルト値）
    let args: Vec<String> = std::env::args().collect();
    let master_url = if args.len() > 1 {
        args[1].clone()
    } else {
        DEFAULT_MASTER_URL.to_string()
    };

    info!("Starting GARADAMA agent with ID: {}", agent_id);
    info!("Connecting to master at: {}", master_url);

    let url = Url::parse(&master_url)?;
    let (ws_stream, _) = connect_async(url).await?;
    info!("WebSocket connection established with master");

    let (sink, stream): (WsSink, WsStream) = ws_stream.split();
    let sink = Arc::new(Mutex::new(sink));
    let connections: ConnectionMap = Arc::new(Mutex::new(HashMap::new()));

    // 初期化リクエスト送信
    handle_init_request(&agent_id, sink.clone()).await?;

    // イベントループ開始
    event_loop(stream, sink, connections).await?;

    Ok(())
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct GeoData {
    ip: String,
    country: String,
    country_iso: String,
    region_name: String,
    city: String,
    latitude: f64,
    longitude: f64,
    asn: String,
    asn_org: String,
    #[serde(rename = "user_agent")]
    user_agent: UserAgent,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct UserAgent {
    product: String,
    version: String,
    raw_value: String,
}
