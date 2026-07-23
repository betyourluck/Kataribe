//! Kataribe ノックサーバー (spec 23 Phase C)。
//!
//! 責務は 3 つだけ: ①部屋コードの発行 ②同じ部屋のピアへの SDP/ICE 中継
//! ③TURN 一時クレデンシャルの発行。**ゲームの語彙を一切知らない** — プレイの中身
//! (game_request / view DTO / 音声) は DataChannel/DTLS の中をピア同士で流れ、
//! このサーバーは覗けない (覗けるのは「誰と誰が繋がったか」だけ)。
//!
//! デプロイ: outcast の docker-compose.prod.yml + Caddy (knock.{DOMAIN} が wss を
//! 素通し)。coturn は同 VPS で 3478/5349 を直接 listen (契約 knock_hosting)。
//!
//! env:
//! - KNOCK_ADDR         listen アドレス (既定 0.0.0.0:5000)
//! - TURN_SECRET        coturn と共有する static-auth-secret (未設定 = TURN 無し・direct のみ)
//! - TURN_URLS          ICE に配る URL 群 (カンマ区切り。例:
//!   "turns:turn.outcasts.jp:5349?transport=tcp,turn:turn.outcasts.jp:3478")
//! - TURN_TTL_SECS      クレデンシャル TTL (既定 300 = 分単位、契約の運用防御①)
//! - ROOM_TTL_SECS      部屋の無活動 TTL (既定 600 = 10 分、契約 room_code)
//! - CREATE_PER_MIN     IP あたりの部屋作成上限/分 (既定 6、契約の運用防御②)
//! - KNOCK_CLIENT_IP    "forwarded" なら X-Forwarded-For 右端を使う (Caddy 1 段の背後。
//!   outcast の CLIENT_IP_SOURCE と同じ前提)。既定 = ソケットの相手

mod msg;
mod ratelimit;
mod registry;
mod turncred;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::connect_info::ConnectInfo;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::Mutex;

use msg::{check_version, ClientMsg, ServerMsg, TurnCred, PROTOCOL_VERSION};
use ratelimit::RateLimiter;
use registry::Registry;

struct Config {
    addr: String,
    turn_secret: Option<String>,
    turn_urls: Vec<String>,
    turn_ttl_secs: u64,
    room_ttl: Duration,
    create_per_min: usize,
    forwarded_ip: bool,
}

impl Config {
    fn from_env() -> Self {
        let num = |k: &str, d: u64| {
            std::env::var(k).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(d)
        };
        Self {
            addr: std::env::var("KNOCK_ADDR").unwrap_or_else(|_| "0.0.0.0:5000".into()),
            turn_secret: std::env::var("TURN_SECRET").ok().filter(|s| !s.trim().is_empty()),
            turn_urls: std::env::var("TURN_URLS")
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            turn_ttl_secs: num("TURN_TTL_SECS", 300),
            room_ttl: Duration::from_secs(num("ROOM_TTL_SECS", 600)),
            create_per_min: num("CREATE_PER_MIN", 6) as usize,
            forwarded_ip: std::env::var("KNOCK_CLIENT_IP")
                .map(|v| v.trim().eq_ignore_ascii_case("forwarded"))
                .unwrap_or(false),
        }
    }

    /// TURN クレデンシャルを発行 (TURN_SECRET 未設定なら None = direct のみ)。
    fn make_turn(&self) -> Option<TurnCred> {
        let secret = self.turn_secret.as_deref()?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        Some(turncred::turn_credential(secret, &self.turn_urls, self.turn_ttl_secs, now))
    }
}

struct AppState {
    registry: Mutex<Registry>,
    limiter: Mutex<RateLimiter>,
    cfg: Config,
}

#[tokio::main]
async fn main() {
    let cfg = Config::from_env();
    let addr = cfg.addr.clone();
    let state = Arc::new(AppState {
        registry: Mutex::new(Registry::new(cfg.room_ttl)),
        limiter: Mutex::new(RateLimiter::new(Duration::from_secs(60), cfg.create_per_min)),
        cfg,
    });

    // 定期 sweep (lazy 判定と二層 — 触られない部屋もメモリから確実に消す)。
    {
        let st = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                let now = Instant::now();
                st.registry.lock().await.sweep(now);
                st.limiter.lock().await.sweep(now);
            }
        });
    }

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state);

    println!("[knock] listening on {addr} (protocol v{PROTOCOL_VERSION})");
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind できる");
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .expect("serve");
}

async fn ws_handler(
    State(st): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let ip = client_ip(&headers, addr, st.cfg.forwarded_ip);
    ws.on_upgrade(move |socket| connection(st, socket, ip))
}

/// クライアント IP。Caddy 1 段の背後では X-Forwarded-For の**右端** = 実クライアント
/// (outcast C-010 と同じ前提。app を外部公開しないことで XFF 偽装を防ぐ構図も同じ)。
fn client_ip(headers: &HeaderMap, sock: SocketAddr, forwarded: bool) -> String {
    if forwarded {
        if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(last) = xff.split(',').next_back().map(str::trim).filter(|s| !s.is_empty())
            {
                return last.to_string();
            }
        }
    }
    sock.ip().to_string()
}

/// 1 接続 = 高々 1 つの (部屋, ピア)。閉じたら leave して peer_left を配る。
async fn connection(st: Arc<AppState>, mut socket: WebSocket, ip: String) {
    let (tx, mut rx) = unbounded_channel::<String>();
    // (room_code, peer_id) — create/join が確定してから Some。
    let mut session: Option<(String, String)> = None;

    loop {
        tokio::select! {
            out = rx.recv() => {
                match out {
                    Some(json) => {
                        if socket.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break, // 台帳から送信口が差し替えられた (再 knock で置換) = この接続は用済み
                }
            }
            inbound = socket.recv() => {
                let Some(Ok(m)) = inbound else { break };
                let text = match m {
                    Message::Text(t) => t.to_string(),
                    Message::Close(_) => break,
                    _ => continue, // ping/pong/binary は無視 (シグナリングはテキストのみ)
                };
                let reply = handle(&st, &tx, &mut session, &ip, &text).await;
                if let Some(r) = reply {
                    if socket.send(Message::Text(r.to_json().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    if let Some((code, peer)) = session {
        st.registry.lock().await.leave(&code, &peer, Instant::now());
    }
}

/// 受信 1 件の処理。返り値 = 本人へ直接返す応答 (中継・通知は registry が tx へ流す)。
async fn handle(
    st: &AppState,
    tx: &registry::PeerTx,
    session: &mut Option<(String, String)>,
    ip: &str,
    text: &str,
) -> Option<ServerMsg> {
    let parsed: ClientMsg = match serde_json::from_str(text) {
        Ok(p) => p,
        Err(e) => {
            return Some(ServerMsg::Error {
                code: "bad_message".into(),
                message: format!("メッセージを解釈できません: {e}"),
            })
        }
    };
    let now = Instant::now();
    match parsed {
        ClientMsg::Create { protocol_version } => {
            if let Err(e) = check_version(protocol_version) {
                return Some(e);
            }
            if session.is_some() {
                return Some(already_in_room());
            }
            if !st.limiter.lock().await.allow(ip, now) {
                return Some(ServerMsg::Error {
                    code: "rate_limited".into(),
                    message: "部屋の作成が多すぎます。少し待ってから試してください".into(),
                });
            }
            let (code, peer_id) = st.registry.lock().await.create(tx.clone(), now);
            *session = Some((code.clone(), peer_id.clone()));
            Some(ServerMsg::Created {
                room_code: code,
                peer_id,
                protocol_version: PROTOCOL_VERSION,
                turn: st.cfg.make_turn(),
            })
        }
        ClientMsg::Join { room_code, protocol_version, peer_id } => {
            if let Err(e) = check_version(protocol_version) {
                return Some(e);
            }
            if session.is_some() {
                return Some(already_in_room());
            }
            match st.registry.lock().await.join(&room_code, peer_id, tx.clone(), now) {
                Ok(ok) => {
                    *session = Some((room_code, ok.peer_id.clone()));
                    Some(ServerMsg::Joined {
                        peer_id: ok.peer_id,
                        peers: ok.others,
                        protocol_version: PROTOCOL_VERSION,
                        turn: st.cfg.make_turn(),
                    })
                }
                Err(e) => Some(e.to_msg()),
            }
        }
        ClientMsg::Signal { kind, to, payload } => {
            let Some((code, me)) = session.as_ref() else {
                return Some(ServerMsg::Error {
                    code: "not_in_room".into(),
                    message: "先に create か join をしてください".into(),
                });
            };
            match st.registry.lock().await.signal(code, me, kind, &to, payload, now) {
                Ok(()) => None, // 中継成功は静か (往復を増やさない)
                Err(e) => Some(e.to_msg()),
            }
        }
    }
}

fn already_in_room() -> ServerMsg {
    ServerMsg::Error {
        code: "already_in_room".into(),
        message: "この接続は既に部屋に属しています (1 接続 1 部屋)".into(),
    }
}
