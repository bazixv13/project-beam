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

// Grace periods before a transport loss is announced to the remaining peer.
// Phones routinely suspend the socket for seconds (e.g. native file picker
// overlay); announcing instantly causes false disconnects on both sides.
const LEAVE_GRACE_NORMAL_SECS: u64 = 5;
// Extended grace while the lost peer announced an open file picker
// (peer-busy): its JS timers are frozen, so it cannot rejoin until the
// picker resolves and the OS unsuspends the tab.
const LEAVE_GRACE_BUSY_SECS: u64 = 150;

type PeerSender = mpsc::UnboundedSender<Message>;

struct RoomManager {
    // Map of roomId -> list of (conn_id, sender)
    rooms: DashMap<String, Vec<(usize, PeerSender)>>,
    // Map of roomId -> conn_id of the peer with an open native file picker
    room_busy: DashMap<String, usize>,
}

impl RoomManager {
    fn new() -> Self {
        Self {
            rooms: DashMap::new(),
            room_busy: DashMap::new(),
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

        // A (re)join restores continuity: any stale picker-busy flag for this
        // room is obsolete, and a pending delayed leave notice will suppress
        // itself when it sees the room full again.
        self.room_busy.remove(room_id);

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

    // Track native file picker presence announced by clients. The message
    // itself is still relayed to the peer by the caller afterwards.
    fn note_picker_state(&self, room_id: &str, conn_id: usize, picking: bool) {
        if picking {
            println!("[ROOM] Client #{} opened file picker in room {}", conn_id, room_id);
            self.room_busy.insert(room_id.to_string(), conn_id);
        } else {
            let is_owner = self.room_busy.get(room_id).map(|r| *r == conn_id).unwrap_or(false);
            if is_owner {
                println!("[ROOM] Client #{} closed file picker in room {}", conn_id, room_id);
                self.room_busy.remove(room_id);
            }
        }
    }

    fn notify_leave(&self, room_id: &str, conn_id: usize) {
        let notification = serde_json::json!({
            "type": "user-left",
            "sender": conn_id.to_string()
        });
        let msg_text = notification.to_string();
        if let Some(peers) = self.rooms.get(room_id) {
            for (id, peer_tx) in peers.iter() {
                if *id != conn_id {
                    let _ = peer_tx.send(Message::Text(msg_text.clone()));
                }
            }
        }
    }

    // Delayed notice for transport loss. Suppressed when the room is full
    // again (peer rejoining after a transient drop), which is the common
    // phone-picker-suspend case.
    fn fire_leave_notice(&self, room_id: &str, conn_id: usize) {
        self.room_busy.remove(room_id);
        let full = self.rooms.get(room_id).map(|e| e.len() >= 2).unwrap_or(false);
        if full {
            println!("[ROOM] Client #{} is back in room {}, suppressing leave notice", conn_id, room_id);
            return;
        }
        println!("[ROOM] Client #{} left room {} (confirmed after grace)", conn_id, room_id);
        self.notify_leave(room_id, conn_id);
    }

    fn schedule_leave_notice(rooms: Arc<RoomManager>, room_id: String, conn_id: usize, delay_secs: u64) {
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
            rooms.fire_leave_notice(&room_id, conn_id);
        });
    }

    // Remove conn from all rooms WITHOUT notifying. Returns the affected
    // rooms with the appropriate leave-notice grace for each.
    fn drop_connection(&self, conn_id: usize) -> Vec<(String, u64)> {
        let busy = &self.room_busy;
        let mut dropped = Vec::new();
        self.rooms.retain(|room_id, peers| {
            let before = peers.len();
            peers.retain(|(id, _)| *id != conn_id);
            if peers.len() < before {
                println!("[ROOM] Client #{} connection dropped from room {}", conn_id, room_id);
                let picking = busy.get(room_id).map(|r| *r == conn_id).unwrap_or(false);
                let grace = if picking { LEAVE_GRACE_BUSY_SECS } else { LEAVE_GRACE_NORMAL_SECS };
                dropped.push((room_id.clone(), grace));
            }
            !peers.is_empty()
        });
        dropped
    }

    fn leave_all_rooms(&self, conn_id: usize) {
        self.rooms.retain(|room_id, peers| {
            let before = peers.len();
            peers.retain(|(id, _)| *id != conn_id);
            if peers.len() < before {
                println!("[ROOM] Client #{} left room {}", conn_id, room_id);
                self.notify_leave(room_id, conn_id);
            }
            !peers.is_empty()
        });
        // Explicit leave ends any picker-busy state for this connection.
        self.room_busy.retain(|_, busy_id| *busy_id != conn_id);
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
                        // Snoop file picker presence (still relayed below):
                        // extends the leave-notice grace if this socket dies.
                        if header.msg_type == "peer-busy" {
                            rooms.note_picker_state(&r_id, conn_id, true);
                        } else if header.msg_type == "peer-back" {
                            rooms.note_picker_state(&r_id, conn_id, false);
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
    // Slot is freed immediately so a rejoin works, but the remaining peer
    // is only notified after a grace period (suppressed on quick rejoin).
    for (room_id, grace) in rooms.drop_connection(conn_id) {
        RoomManager::schedule_leave_notice(rooms.clone(), room_id, conn_id, grace);
    }
    ping_task.abort();
    forward_task.abort();
}
