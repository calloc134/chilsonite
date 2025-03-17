use serde::{Deserialize, Serialize};

/// エージェントとクライアント間でやり取りするメッセージのペイロード定義
#[derive(Serialize, Deserialize, Debug)]
pub enum Payload {
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

    // Added new variant for command messages
    #[serde(rename = "command")]
    Command { command: String },
}
