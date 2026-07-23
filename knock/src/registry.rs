//! 部屋の台帳 (純ロジック)。時計 (`Instant`) は全メソッドで注入 = テスト可能。
//!
//! - 部屋コード = base62 22 桁 (~131bit、推測参加を計算量で遮断。契約 room_code)。
//! - TTL 10 分 (既定)・**あらゆる接続イベントで延長** — WebRTC は切断が日常なので、
//!   同一コードでの再 knock (既存 participant の張り直し) を TTL の間ずっと受ける。
//! - ピアが全員去っても部屋は TTL まで残す (全断 → 全員が張り直す、を生かすため)。
//! - サーバーはゲームの語彙を知らない。ここに在るのは code / peer / 中継だけ。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rand::Rng as _;
use tokio::sync::mpsc::UnboundedSender;

use crate::msg::{ServerMsg, SignalKind};

/// ピアへの送信口 (シリアライズ済み JSON 1 行)。WS 書き込みタスクが吸い出す。
pub type PeerTx = UnboundedSender<String>;

const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
/// 部屋コード長 (契約 room_code = base62 22 桁 ≈ 131bit)。
pub const ROOM_CODE_LEN: usize = 22;
/// 部屋の定員 (2〜3 人卓 + 余裕。無制限にしない = リレー悪用の面を増やさない)。
pub const MAX_PEERS: usize = 8;

#[derive(Debug, PartialEq, Eq)]
pub enum KnockError {
    RoomNotFound,
    RoomFull,
    PeerNotFound,
}

impl KnockError {
    pub fn to_msg(&self) -> ServerMsg {
        let (code, message) = match self {
            KnockError::RoomNotFound => (
                "room_not_found",
                "部屋が見つかりません (コード誤りか、期限切れです)",
            ),
            KnockError::RoomFull => ("room_full", "部屋が満員です"),
            KnockError::PeerNotFound => ("peer_not_found", "相手が見つかりません (切断済み)"),
        };
        ServerMsg::Error { code: code.into(), message: message.into() }
    }
}

struct Room {
    last_activity: Instant,
    peers: HashMap<String, PeerTx>,
}

pub struct Registry {
    rooms: HashMap<String, Room>,
    ttl: Duration,
}

/// join の結果 (本人に返す素材)。
#[derive(Debug)]
pub struct JoinOk {
    // rejoined は main では通知種別 (peer_rejoined) が既に registry 内で配られるため未読だが、
    // 呼び出し側が区別したくなる情報として構造体に残す (テストは読む)。
    pub peer_id: String,
    /// 先に居る相手 (offer を出す宛先)。
    pub others: Vec<String>,
    /// 再 knock (張り直し) だったか。
    #[allow(dead_code)]
    pub rejoined: bool,
}

impl Registry {
    pub fn new(ttl: Duration) -> Self {
        Self { rooms: HashMap::new(), ttl }
    }

    /// 部屋を開く (ホスト)。戻り = (部屋コード, ホストの peer_id)。
    pub fn create(&mut self, tx: PeerTx, now: Instant) -> (String, String) {
        let code = loop {
            let c = room_code();
            if !self.rooms.contains_key(&c) {
                break c;
            }
        };
        let peer_id = new_peer_id();
        let mut peers = HashMap::new();
        peers.insert(peer_id.clone(), tx);
        self.rooms.insert(code.clone(), Room { last_activity: now, peers });
        (code, peer_id)
    }

    /// 入室 (ゲスト) / 再 knock (`reuse` = 前回の peer_id)。
    /// 既存メンバーへ peer_joined / peer_rejoined を配る (再シグナリングの合図)。
    pub fn join(
        &mut self,
        code: &str,
        reuse: Option<String>,
        tx: PeerTx,
        now: Instant,
    ) -> Result<JoinOk, KnockError> {
        let room = self.live_room(code, now)?;
        let rejoined = reuse.as_ref().is_some_and(|id| room.peers.contains_key(id));
        if !rejoined && room.peers.len() >= MAX_PEERS {
            return Err(KnockError::RoomFull);
        }
        let peer_id = if rejoined {
            reuse.expect("rejoined は reuse が Some の時のみ真")
        } else {
            new_peer_id()
        };
        let others: Vec<String> =
            room.peers.keys().filter(|k| **k != peer_id).cloned().collect();
        // 張り直しは古い送信口を差し替える (切断済みチャネルに送り続けない)。
        room.peers.insert(peer_id.clone(), tx);
        let note = if rejoined {
            ServerMsg::PeerRejoined { peer_id: peer_id.clone() }
        } else {
            ServerMsg::PeerJoined { peer_id: peer_id.clone() }
        };
        broadcast(room, &peer_id, &note);
        Ok(JoinOk { peer_id, others, rejoined })
    }

    /// SDP/ICE を宛先ピアへ中継する (payload は非解釈)。
    pub fn signal(
        &mut self,
        code: &str,
        from: &str,
        kind: SignalKind,
        to: &str,
        payload: serde_json::Value,
        now: Instant,
    ) -> Result<(), KnockError> {
        let room = self.live_room(code, now)?;
        let tx = room.peers.get(to).ok_or(KnockError::PeerNotFound)?;
        let _ = tx.send(ServerMsg::Signal { kind, from: from.to_string(), payload }.to_json());
        Ok(())
    }

    /// 切断 (WS が閉じた)。部屋自体は TTL まで残す = 再 knock の待受 (契約 room_code)。
    pub fn leave(&mut self, code: &str, peer_id: &str, now: Instant) {
        if let Ok(room) = self.live_room(code, now) {
            if room.peers.remove(peer_id).is_some() {
                broadcast(room, peer_id, &ServerMsg::PeerLeft { peer_id: peer_id.to_string() });
            }
        }
    }

    /// 期限切れの部屋を落とす (定期タスク + 各操作の lazy 判定の二層)。
    pub fn sweep(&mut self, now: Instant) {
        let ttl = self.ttl;
        self.rooms.retain(|_, r| now.duration_since(r.last_activity) < ttl);
    }

    /// 生きている部屋を触る (期限は lazy 判定・触れたら活動時刻を延長)。
    fn live_room(&mut self, code: &str, now: Instant) -> Result<&mut Room, KnockError> {
        let expired = self
            .rooms
            .get(code)
            .is_some_and(|r| now.duration_since(r.last_activity) >= self.ttl);
        if expired {
            self.rooms.remove(code);
        }
        let room = self.rooms.get_mut(code).ok_or(KnockError::RoomNotFound)?;
        room.last_activity = now;
        Ok(room)
    }
}

fn broadcast(room: &Room, except: &str, msg: &ServerMsg) {
    let json = msg.to_json();
    for (id, tx) in &room.peers {
        if id != except {
            let _ = tx.send(json.clone());
        }
    }
}

/// 部屋コード: base62 22 桁 (~131bit。OS 乱数由来の CSPRNG = rand::rng)。
fn room_code() -> String {
    let mut r = rand::rng();
    (0..ROOM_CODE_LEN).map(|_| BASE62[r.random_range(0..BASE62.len())] as char).collect()
}

/// ピア id (16 hex。部屋内で一意なら足りる — 会話の宛先ラベルであって秘密ではない)。
fn new_peer_id() -> String {
    let mut r = rand::rng();
    (0..16).map(|_| char::from_digit(r.random_range(0..16u32), 16).expect("16 進 1 桁")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

    fn ch() -> (PeerTx, UnboundedReceiver<String>) {
        unbounded_channel()
    }
    fn drain(rx: &mut UnboundedReceiver<String>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(m) = rx.try_recv() {
            out.push(m);
        }
        out
    }

    /// 【部屋コードの形】base62 22 桁 (契約 room_code)。2 つ生成して一致しない
    /// (衝突しないことの証明ではなく、生成器が固定値でないことの smoke)。
    #[test]
    fn room_code_is_22_base62_chars() {
        let a = room_code();
        let b = room_code();
        assert_eq!(a.chars().count(), ROOM_CODE_LEN);
        assert!(a.bytes().all(|c| BASE62.contains(&c)), "base62 のみ: {a}");
        assert_ne!(a, b);
    }

    /// 【create → join → 中継 → leave の一巡】ホストは peer_joined を受け、offer は宛先だけに
    /// from 付きで届き、切断は peer_left で伝わる。サーバーは payload を解釈しない。
    #[test]
    fn create_join_relay_and_leave_roundtrip() {
        let mut reg = Registry::new(Duration::from_secs(600));
        let t0 = Instant::now();
        let (htx, mut hrx) = ch();
        let (gtx, mut grx) = ch();

        let (code, host) = reg.create(htx, t0);
        let ok = reg.join(&code, None, gtx, t0).expect("入室できる");
        assert!(!ok.rejoined);
        assert_eq!(ok.others, vec![host.clone()], "先に居る相手 = offer の宛先");
        assert!(
            drain(&mut hrx).iter().any(|m| m.contains("peer_joined")),
            "ホストに入室が伝わる"
        );

        // guest → host へ offer 中継。ゲスト自身には流れない。
        reg.signal(&code, &ok.peer_id, SignalKind::Offer, &host, serde_json::json!({"sdp": "x"}), t0)
            .unwrap();
        let host_got = drain(&mut hrx);
        assert!(
            host_got.iter().any(|m| m.contains("offer") && m.contains(&ok.peer_id)),
            "offer が from 付きで届く: {host_got:?}"
        );
        assert!(drain(&mut grx).is_empty(), "送信者には返らない");

        // 未知の宛先は peer_not_found。
        assert_eq!(
            reg.signal(&code, &host, SignalKind::Answer, "nobody", serde_json::json!({}), t0),
            Err(KnockError::PeerNotFound)
        );

        reg.leave(&code, &ok.peer_id, t0);
        assert!(drain(&mut hrx).iter().any(|m| m.contains("peer_left")));
    }

    /// 【TTL と延長】無活動 TTL で部屋は消え (join = room_not_found)、活動イベントは
    /// TTL を延長する。ピアが全員去っても TTL までは再 knock を待つ。
    #[test]
    fn ttl_expires_rooms_and_activity_extends() {
        let ttl = Duration::from_secs(600);
        let mut reg = Registry::new(ttl);
        let t0 = Instant::now();
        let (htx, _hrx) = ch();
        let (code, host) = reg.create(htx, t0);

        // 活動 (t0+300 の signal 試行) は失敗しても部屋に触れる = 延長。
        let _ = reg.signal(&code, &host, SignalKind::Offer, "x", serde_json::json!({}), t0 + Duration::from_secs(300));
        // 旧起点なら期限切れの t0+700 でも、延長済みなので生きている。
        let (g1, _g1rx) = ch();
        assert!(reg.join(&code, None, g1, t0 + Duration::from_secs(700)).is_ok());

        // そこから無活動で TTL 経過 → lazy 判定で消える。
        let (g2, _g2rx) = ch();
        assert_eq!(
            reg.join(&code, None, g2, t0 + Duration::from_secs(700) + ttl).unwrap_err(),
            KnockError::RoomNotFound
        );

        // sweep も同じ判定 (定期タスク側の経路)。
        let (htx2, _r) = ch();
        let (code2, _h2) = reg.create(htx2, t0);
        reg.sweep(t0 + ttl);
        assert!(!reg.rooms.contains_key(&code2), "sweep で期限切れが落ちる");
    }

    /// 【再 knock = 張り直し】同じ peer_id で join し直すと identity を保ったまま送信口が
    /// 差し替わり、他メンバーには peer_rejoined (再シグナリングの合図) が流れる。
    /// 定員判定は張り直しを新規として数えない。
    #[test]
    fn re_knock_replaces_transport_and_keeps_identity() {
        let mut reg = Registry::new(Duration::from_secs(600));
        let t0 = Instant::now();
        let (htx, mut hrx) = ch();
        let (g1tx, _g1rx) = ch();
        let (code, host) = reg.create(htx, t0);
        let first = reg.join(&code, None, g1tx, t0).unwrap();
        drain(&mut hrx);

        // 切断 (leave は来ないまま = ネット断) → 新しいチャネルで再 knock。
        let (g2tx, mut g2rx) = ch();
        let again = reg
            .join(&code, Some(first.peer_id.clone()), g2tx, t0 + Duration::from_secs(30))
            .unwrap();
        assert!(again.rejoined);
        assert_eq!(again.peer_id, first.peer_id, "identity は保たれる");
        assert!(
            drain(&mut hrx).iter().any(|m| m.contains("peer_rejoined")),
            "既存メンバーに張り直しが伝わる"
        );

        // 以後の中継は新しい送信口に届く。
        reg.signal(&code, &host, SignalKind::Answer, &again.peer_id, serde_json::json!({}), t0 + Duration::from_secs(31))
            .unwrap();
        assert!(drain(&mut g2rx).iter().any(|m| m.contains("answer")));
    }

    /// 【定員】MAX_PEERS で room_full (2〜3 人卓 + 余裕。無制限にしない)。
    #[test]
    fn room_caps_at_max_peers() {
        let mut reg = Registry::new(Duration::from_secs(600));
        let t0 = Instant::now();
        let (htx, _hrx) = ch();
        let (code, _host) = reg.create(htx, t0);
        for _ in 0..(MAX_PEERS - 1) {
            let (tx, _rx) = ch();
            reg.join(&code, None, tx, t0).expect("定員内");
        }
        let (tx, _rx) = ch();
        assert_eq!(reg.join(&code, None, tx, t0).unwrap_err(), KnockError::RoomFull);
    }
}
