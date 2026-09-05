use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
    routing::get,
    Router,
};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::sync::mpsc;
use tower_http::{
    compression::CompressionLayer,
    cors::{Any, CorsLayer},
    services::ServeDir,
};

static NEXT_CONN_ID: AtomicUsize = AtomicUsize::new(1);

type PeerSender = mpsc::UnboundedSender<Message>;

struct RoomManager {
    // Map of roomId -> list of (conn_id, sender)
    rooms: DashMap<String, Vec<(usize, PeerSender)>>,
}

impl RoomManager {
    fn new() -> Self {
        Self {
            rooms: DashMap::new(),
        }
    }

    fn join_room(&self, room_id: &str, conn_id: usize, sender: PeerSender) {
        println!("[ROOM] Client #{} joining room: {}", conn_id, room_id);
        let mut entry = self.rooms.entry(room_id.to_string()).or_default();
        entry.retain(|(id, tx)| *id != conn_id && !tx.is_closed());

        // A room strictly connects 2 peers. If stale peers remain, keep at most 1 before adding new.
        if entry.len() > 1 {
            let keep = entry.pop().unwrap();
            entry.clear();
            entry.push(keep);
        }

        let existing_peer = entry.first().map(|(id, tx)| (*id, tx.clone()));
        entry.push((conn_id, sender.clone()));

        let peers_count = entry.len();
        println!("[ROOM] Room {} now has {} peer(s)", room_id, peers_count);

        if let Some((other_id, other_tx)) = existing_peer {
            // Notify existing peer that new peer joined (existing peer initiates WebRTC offer)
            let msg_for_existing = serde_json::json!({
                "type": "user-joined",
                "sender": conn_id.to_string(),
                "initiator": true
            }).to_string();
            println!("[NOTIFY] Notifying existing peer #{} that peer #{} joined room {}", other_id, conn_id, room_id);
            let _ = other_tx.send(Message::Text(msg_for_existing));

            // ALSO notify the newly joined peer that other_id is already waiting in the room!
            let msg_for_new = serde_json::json!({
                "type": "user-joined",
                "sender": other_id.to_string(),
                "initiator": false
            }).to_string();
            println!("[NOTIFY] Notifying new peer #{} that peer #{} is in room {}", conn_id, other_id, room_id);
            let _ = sender.send(Message::Text(msg_for_new));
        }
    }

    fn broadcast_to_room(&self, room_id: &str, sender_id: usize, msg: Message) {
        if let Some(mut entry) = self.rooms.get_mut(room_id) {
            entry.retain(|(id, peer_tx)| {
                if *id == sender_id {
                    return true;
                }
                peer_tx.send(msg.clone()).is_ok()
            });
        }
    }

    fn leave_all_rooms(&self, conn_id: usize) {
        self.rooms.retain(|room_id, peers| {
            let before = peers.len();
            peers.retain(|(id, _)| *id != conn_id);
            if peers.len() < before {
                println!("[ROOM] Client #{} left room {}", conn_id, room_id);
                let notification = serde_json::json!({
                    "type": "user-left",
                    "sender": conn_id.to_string()
                });
                let msg_text = notification.to_string();
                for (id, peer_tx) in peers.iter() {
                    if *id != conn_id {
                        let _ = peer_tx.send(Message::Text(msg_text.clone()));
                    }
                }
            }
            !peers.is_empty()
        });
    }
}

#[derive(Clone)]
struct AppState {
    rooms: Arc<RoomManager>,
}

#[derive(Deserialize, Debug)]
struct ClientMessageHeader {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(rename = "roomId")]
    room_id: Option<String>,
}

#[tokio::main]
async fn main() {
    let dist_dir = std::env::var("DIST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./dist"));

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3001);

    let state = AppState {
        rooms: Arc::new(RoomManager::new()),
    };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let compression = CompressionLayer::new();

    let serve_dir = ServeDir::new(&dist_dir).fallback(ServeDir::new(&dist_dir));

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .fallback_service(serve_dir)
        .layer(compression)
        .layer(cors)
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!(">>> P2P Beam High-Performance Native Server running on http://0.0.0.0:{}", port);
    println!(">>> Serving static assets from: {:?}", dist_dir);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
    println!("[WS] Client #{} connected", conn_id);
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    let forward_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    let ping_tx = tx.clone();
    let ping_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
        loop {
            interval.tick().await;
            if ping_tx.send(Message::Ping(vec![])).is_err() {
                break;
            }
        }
    });

    let rooms = state.rooms.clone();
    let mut current_room: Option<String> = None;

    while let Some(Ok(msg)) = ws_receiver.next().await {
        match msg {
            Message::Text(ref text) => {
                if let Ok(header) = serde_json::from_str::<ClientMessageHeader>(text) {
                    if let Some(r_id) = header.room_id {
                        current_room = Some(r_id.clone());
                        if header.msg_type == "join-room" {
                            rooms.join_room(&r_id, conn_id, tx.clone());
                            continue;
                        }
                        if header.msg_type == "leave-room" {
                            rooms.leave_all_rooms(conn_id);
                            current_room = None;
                            continue;
                        }
                    }
                }
                
                // Relay all other room messages (WebRTC signaling, ICE candidates, fallback data)
                if let Some(ref r_id) = current_room {
                    rooms.broadcast_to_room(r_id, conn_id, msg);
                }
            }
            Message::Binary(_) => {
                if let Some(ref r_id) = current_room {
                    rooms.broadcast_to_room(r_id, conn_id, msg);
                }
            }
            Message::Close(_) => break,
            Message::Pong(_) => {
                // Client alive, pong received from browser
            }
            _ => {}
        }
    }

    println!("[WS] Client #{} disconnected", conn_id);
    rooms.leave_all_rooms(conn_id);
    ping_task.abort();
    forward_task.abort();
}
