# AGENTS.md — AI Instructions for Project BEAM

> **Notice to AI Agents:** You are assisting a developer working on **Project BEAM** (an ultra-lean, high-throughput WebRTC & native Rust file transfer engine). Read this document carefully before proposing, modifying, or testing any code.

---

## 1. Project Mission & Constraints

- **Objective:** Maximum throughput, zero-bloat file transfer between browsers with 0ms room connection latency and high fault-tolerance.
- **Production Environment:** Oracle Cloud Rocky Linux 9 instance (`1 GB RAM, 1 vCPU core`).
- **Critical Resource Constraint:** Memory is severely constrained on production. The backend server must remain minimal (~1.5 MB RAM footprint) and never buffer entire files in memory. All streaming must be zero-copy or chunk-streamed in RAM.
- **Public Endpoints:** `https://beam.hs.vc/` and `https://filetrans.duckdns.org/` (Served via Caddy reverse proxy to `127.0.0.1:3001`).

---

## 2. Codebase Architecture

```
project-beam/
├── client/              # React 19 + Vite Frontend
│   ├── src/
│   │   ├── App.jsx      # UI layout, room orchestration, theme, transfer strips
│   │   ├── webrtc.js    # Core transfer engine: WebRTC DataChannel + WS relay fallback
│   │   ├── index.css    # High-contrast brutalist monochrome styling
│   │   ├── QRScanner.jsx # Camera-based QR code reader
│   │   └── QRCodeDisplay.jsx # SVG QR generator
│   ├── package.json
│   └── vite.config.js
└── server/              # Native Rust Signaling & Binary Relay Server
    ├── src/
    │   └── main.rs      # Axum WebSocket server, room registry, departure alerts
    ├── Cargo.toml
    └── Cargo.lock
```

---

## 3. Strict Architectural Rules for AI Agents

When modifying this repository, AI agents must strictly follow these invariants:

### A. Frontend & Transfer Engine (`client/src/webrtc.js`)
1. **Zero-Copy Uint8Array Slicing:**
   - Never use `blockBuffer.slice(...)` for chunk generation; always use typed buffer views: `new Uint8Array(blockBuffer, blockPos, chunkLen)`. This avoids thousands of GC allocations per gigabyte.
2. **WebRTC Direct Upgrade Invariants:**
   - Only switch `mode = 'webrtc'` when **both** ICE state is verified (`connected` or `completed`) **and** `dataChannel.readyState === 'open'`.
   - Never assume DataChannel `onopen` implies a working connection; in some browsers SCTP initializes before ICE candidates finish checking.
3. **Graceful Downgrade & Mid-Transfer Fallback:**
   - If WebRTC checks fail, time out (8s limit), or the DataChannel throws `Restricted`/errors, the client must trigger `downgradeToRelay(reason, notifyPeer)`.
   - Both peers must be informed via `p2p-downgrade` WebSocket control packets so transport modes remain synchronized.
   - If DataChannel drops mid-flight in `sendFile`, catch the error immediately and continue streaming remaining chunks over WebSocket (`this.socket`) without terminating or restarting the transfer.
4. **Heartbeat & Liveness (Anti-Flicker):**
   - Refresh `lastPeerHeartbeat` on **every** incoming chunk or control message.
   - Never trigger disconnects while active transfers (`isSending || receiveFileId`) have an open WebSocket socket.
   - Silence threshold is 12 seconds minimum.
5. **Disk-Spilled Blob Flushing:**
   - Receiver must batch incoming chunks into 16 MB disk-spilled `Blob` arrays to keep browser JavaScript heap under 16 MB even during multi-gigabyte transfers on mobile devices.

### B. Backend (`server/src/main.rs`)
1. **Zero State Persistence:**
   - No database, no disk caching. All rooms and connections exist only in memory via thread-safe `DashMap`.
2. **Departure Notification:**
   - If a peer disconnects, navigates away, or closes the socket, broadcast `{"type": "user-left", "sender": id}` to any remaining room participant immediately.
3. **Binary Relay Fallback:**
   - Relay binary chunks directly across unbounded channels without unpacking, deserializing, or cloning payload buffers.

---

## 4. Local Build & Test Commands

Before committing any changes, the AI agent must ensure both components compile cleanly:

```bash
# 1. Test Client Build
cd client
npm install
npm run build

# 2. Test Server Compilation
cd ../server
cargo check
# Or build release binary:
cargo build --release
```

---

## 5. Directions for AI Agents to Commit & Push Changes

When you (the AI agent) have finished implementing and verifying code changes for the contributor, execute or instruct the contributor to execute the following Git workflow:

### Step 1: Verify Status
Ensure no temporary, debug, or build output files (`dist/`, `target/`, `node_modules/`, `*.log`) are being tracked:
```bash
git status
```

### Step 2: Stage Modified Files
Stage the intentional changes cleanly:
```bash
git add client/ server/ README.md
```

### Step 3: Format Commit Message
Write a clear, conventional commit message describing the exact bugfix or feature:
```bash
git commit -m "fix(transfer): description of changes made"
```

### Step 4: Push to Remote
Push the branch to the shared repository:
```bash
# If on a feature branch:
git push origin <branch-name>

# If on main:
git push origin main
```

---

## 6. Production Deployment Instructions (Reference)

If the contributor instructs you to deploy updates to the production server:

```bash
# Deploy Frontend Assets:
cd client && npm run build
scp -r dist/* oracle-rocky:/opt/p2p-beam/dist/

# Deploy Backend Binary:
cd ../server && cargo build --release
scp target/release/p2p-server oracle-rocky:/opt/p2p-beam/p2p-server.new
ssh oracle-rocky "mv /opt/p2p-beam/p2p-server.new /opt/p2p-beam/p2p-server && sudo systemctl restart p2p-beam.service"
```
