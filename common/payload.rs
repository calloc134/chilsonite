/// エージェントとクライアント間でやり取りするメッセージのペイロード定義
#[derive(Serialize, Deserialize, Debug)]
pub enum Payload {
    #[serde(rename = "init-request")]
    InitRequest {
        pub agent_id: String,
        pub ip: String,
        pub remote_host: String,
        pub country_code: String,
        pub city: String,
        pub region: String,
        pub asn: String,
        pub asn_org: String,
    },
    #[serde(rename = "init-response")]
    InitResponse {
        pub success: bool,
        pub message: Option<String>,
    },
    #[serde(rename = "init-error")]
    InitError { pub error_message: String },
    #[serde(rename = "connect-request")]
    ConnectRequest {
        pub request_id: String,
        pub target_addr: String,
        pub target_port: u16,
        pub agent_id: Option<String>,
        pub address_type: u8,
    },
    #[serde(rename = "connect-response")]
    ConnectResponse {
        pub request_id: String,
        pub success: bool,
    },
    #[serde(rename = "data-chunk-request")]
    DataRequestChunk {
        pub request_id: String,
        pub chunk_id: u32,
        pub data: String,
    },
    #[serde(rename = "data-chunk-response")]
    DataResponseChunk {
        pub request_id: String,
        pub chunk_id: u32,
        pub data: String,
    },
    #[serde(rename = "data-response-transfer-complete")]
    DataResponseTransferComplete {
        pub request_id: String,
        pub success: bool,
        pub error_message: Option<String>,
    },
    #[serde(rename = "data-request-transfer-complete")]
    DataRequestTransferComplete {
        pub request_id: String,
        pub success: bool,
        pub error_message: Option<String>,
    },
    #[serde(rename = "client-disconnect")]
    ClientDisconnect { pub request_id: String },
}
