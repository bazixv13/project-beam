# BEAM — High-Performance P2P File Transfer

> Ultra-lean, zero-cloud peer-to-peer file transfer web application built with **React 19**, **WebRTC**, and a native **Rust (Axum)** signaling & relay engine.

- **Development / Live Server:** [https://filetrans.duckdns.org/](https://filetrans.duckdns.org/) (Auto-deployed via CI/CD on push to `main`)
- **Primary Endpoint:** [https://beam.hs.vc/](https://beam.hs.vc/)
- **WebSocket Endpoint:** `wss://filetrans.duckdns.org/ws`

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
├── .github/workflows/
│   └── deploy.yml       # Automated CI/CD deployment to https://filetrans.duckdns.org/
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
├── AGENTS.md            # AI agent instructions, architectural rules, and contribution directions
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

## 🔄 Automated Deployment

Deployments to the development server at [https://filetrans.duckdns.org/](https://filetrans.duckdns.org/) are fully automated via GitHub Actions (`.github/workflows/deploy.yml`).

Whenever changes are merged or pushed to the `main` branch, the pipeline:
1. Builds the production frontend bundle (`client`).
2. Compiles the native Rust release binary (`server`).
3. Deploys the assets and binary over SSH to the server.
4. Restarts `p2p-beam.service` and executes a health check against the live endpoint.
