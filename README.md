# BEAM — High-Performance P2P File Transfer

> Ultra-lean, zero-cloud peer-to-peer file transfer web application built with **React 19**, **WebRTC**, and a native **Rust (Axum)** signaling & relay engine.

- **Primary URL:** [https://beam.hs.vc/](https://beam.hs.vc/)
- **Fallback URL:** [https://filetrans.duckdns.org/](https://filetrans.duckdns.org/)
- **WebSocket Endpoint:** `wss://beam.hs.vc/ws`
- **Target Host:** Oracle Cloud Rocky Linux 9 instance (`ssh oracle-rocky`), 1 GB RAM, 1 vCPU.

---

## ⚡ Key Capabilities

- **Direct P2P Transfer (WebRTC):** Direct peer-to-peer streaming via RTCDataChannel with end-to-end encryption (DTLS). Data transfers device-to-device without passing through the server.
- **Resilient Relay Fallback (WebSocket):** When direct P2P is blocked by firewalls or symmetric NATs, transfers seamlessly fall back to the native Rust WebSocket relay. If WebRTC drops mid-transfer, in-flight chunks automatically route through WebSocket without terminating the transfer.
- **Zero-Copy Uint8Array Chunking:** Slices native ArrayBuffers directly into SCTP chunks (64 KB) without GC overhead.
- **Ultra-Short 2-Character Rooms:** Fast room connection codes (`0-9, A-Z`, e.g., `4K`, `9B`) with immediate auto-join upon entering the second character.
- **Integrated QR Code Scanner & Sharing:** Fast device pairing by scanning the QR code with a phone or camera.
- **Resumable Transfers:** When a peer disconnects, in-flight progress is preserved. Reconnecting allows resuming directly from the acknowledged byte offset.
- **Brutalist Monochrome Design:** Clean, flat high-contrast design optimized for desktop and mobile touchscreens.

---

## 🏗️ Project Architecture

```
project-beam/
├── client/              # Frontend (React 19 + Vite)
│   ├── src/
│   │   ├── App.jsx      # UI, room orchestration, theme toggle, bidirectional transfers
│   │   ├── webrtc.js    # Transfer engine: WebRTC, WebSocket fallback, SCTP pipelining
│   │   ├── index.css    # Monochrome brutalist design
│   │   ├── QRScanner.jsx # Camera QR code reader
│   │   └── QRCodeDisplay.jsx # SVG QR generator
│   └── package.json
├── server/              # Native Rust Signaling & Binary Relay Server
│   ├── src/
│   │   └── main.rs      # Axum WebSocket hub, room manager, departure broadcaster
│   ├── Cargo.toml
│   └── Cargo.lock
├── AGENTS.md            # AI agent instructions, rules, and deployment guides
└── README.md
```

---

## 🚀 Local Development

### 1. Client (React 19 + Vite)
```bash
cd client
npm install
npm run dev
```
Runs the Vite development server locally at `http://localhost:5173`.

### 2. Server (Rust Axum)
```bash
cd server
cargo run
```
Runs the native signaling server on `http://127.0.0.1:3001` (WebSocket at `/ws`).

---

## 📦 Building & Production Deployment

### Build Frontend
```bash
cd client
npm run build
```

### Build Native Rust Server
```bash
cd server
cargo build --release
```

### Deploy to Remote Production Server (`oracle-rocky`)
```bash
# 1. Deploy client assets
scp -r client/dist/* oracle-rocky:/opt/p2p-beam/dist/

# 2. Deploy binary & restart service
scp server/target/release/p2p-server oracle-rocky:/opt/p2p-beam/p2p-server.new
ssh oracle-rocky "mv /opt/p2p-beam/p2p-server.new /opt/p2p-beam/p2p-server && sudo systemctl restart p2p-beam.service"
```
