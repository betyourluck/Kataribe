//! ワイヤメッセージ (契約 `Multiplayer.messages` のシグナリング面)。
//!
//! JSON・backend 非依存 (将来 DataChannel 側を Rust webrtc-rs へ移しても形を変えない)。
//! **`sig?` の予約**: 全メッセージに将来の SDP 署名用フィールド `sig` を予約する (v1 未使用)。
//! serde は未知フィールドを黙って受理するので、受信側は今日から `sig` 付きメッセージを
//! 壊れず読める — v1 は emit しないだけ (契約の「予約」= スキーマ上の名前の確保)。
//!
//! ゲームの語彙 (game_request / game_response / game_event / timer_sync / reveal_order) は
//! **ここに現れない** — それらは DataChannel の中をピア同士で流れる。ノックサーバーは
//! ピアを引き合わせるだけで、プレイの中身を見ない (spec 23 の責務境界)。

use serde::{Deserialize, Serialize};

/// join/create で交換するプロトコル版。不一致は接続拒否 (静かな解釈違いを作らない)。
pub const PROTOCOL_VERSION: u32 = 1;

/// クライアント → サーバー。
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// 部屋を開く (ホスト)。
    Create { protocol_version: u32 },
    /// 部屋へ入る (ゲスト)。`peer_id` を添えたら**再 knock** = 既存 participant の張り直し
    /// (中途参加ではない。契約 room_code)。
    Join {
        room_code: String,
        protocol_version: u32,
        #[serde(default)]
        peer_id: Option<String>,
    },
    /// SDP/ICE の中継 (offer / answer / candidate)。payload はサーバー非解釈。
    Signal {
        kind: SignalKind,
        to: String,
        payload: serde_json::Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    Offer,
    Answer,
    Candidate,
}

/// TURN 一時クレデンシャル (coturn `use-auth-secret` / TURN REST API 方式)。
/// username = 失効 unix 秒、credential = base64(HMAC-SHA1(secret, username))。
/// TTL は分単位 (契約 transport の運用防御①) — 再接続で再発行される。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TurnCred {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
    pub ttl_secs: u64,
}

/// サーバー → クライアント。
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// 部屋が開いた (ホストへ)。
    Created {
        room_code: String,
        peer_id: String,
        protocol_version: u32,
        /// TURN_SECRET 未設定 (ローカル開発) なら省略 = direct のみで繋ぐ。
        #[serde(skip_serializing_if = "Option::is_none")]
        turn: Option<TurnCred>,
    },
    /// 入室できた (本人へ)。`peers` は先に居る相手 (offer を出す宛先)。
    Joined {
        peer_id: String,
        peers: Vec<String>,
        protocol_version: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        turn: Option<TurnCred>,
    },
    /// 新しいピアが入った (既存メンバーへ)。
    PeerJoined { peer_id: String },
    /// 既存ピアが張り直した (再 knock。既存メンバーへ — 再シグナリングの合図)。
    PeerRejoined { peer_id: String },
    /// SDP/ICE の中継 (宛先へ)。
    Signal {
        kind: SignalKind,
        from: String,
        payload: serde_json::Value,
    },
    /// ピアが去った (既存メンバーへ)。部屋自体は TTL まで再 knock を待つ。
    PeerLeft { peer_id: String },
    Error { code: String, message: String },
}

impl ServerMsg {
    /// 送信用 JSON (シグナリングの器は常に 1 行テキスト)。
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("ServerMsg は常に serialize 可能")
    }
}

/// プロトコル版の門番。不一致は接続拒否 (契約 protocol_version)。
/// (Err が大きい lint は許容 — 失敗は即応答して接続を畳む一度きりの経路で、性能面の実害なし。)
#[allow(clippy::result_large_err)]
pub fn check_version(v: u32) -> Result<(), ServerMsg> {
    if v == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ServerMsg::Error {
            code: "protocol_mismatch".into(),
            message: format!(
                "プロトコル版が合いません (server={PROTOCOL_VERSION}, client={v})。Kataribe を同じ版に揃えてください"
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 【契約 protocol_version】不一致は接続拒否。`sig` 予約 = 未知フィールドつきでも読める。
    #[test]
    fn version_gate_and_sig_reservation() {
        assert!(check_version(PROTOCOL_VERSION).is_ok());
        assert!(matches!(
            check_version(PROTOCOL_VERSION + 1),
            Err(ServerMsg::Error { code, .. }) if code == "protocol_mismatch"
        ));

        // sig? 付きメッセージも今日から壊れず読める (予約の実体 = 前方互換)。
        let m: ClientMsg = serde_json::from_str(
            r#"{"type":"join","room_code":"abc","protocol_version":1,"sig":"future"}"#,
        )
        .expect("未知フィールド sig は黙って受理される");
        assert!(matches!(m, ClientMsg::Join { room_code, .. } if room_code == "abc"));
    }
}
