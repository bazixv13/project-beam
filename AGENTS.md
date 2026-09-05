# AGENTS.md — AI Instructions for Project BEAM

> **Notice to AI Agents:** You are assisting a contributor working on **Project BEAM** (an ultra-lean, high-throughput WebRTC & native Rust file transfer engine). Read this document carefully before proposing, modifying, or testing any code.
> 
> **Repository:** `https://github.com/bazixv13/project-beam`

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

## 4. Local Build & Test Verification

Before proposing or committing any changes, the AI agent must verify that both components build cleanly:

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

## 5. Directions for AI Agents: How to Contribute & Push Changes

When you (the AI agent) have finished implementing and verifying code changes for the contributor, follow these step-by-step Git instructions:

### Step 1: Clone or Pull the Latest Main
If starting on a new machine:
```bash
git clone https://github.com/bazixv13/project-beam.git
cd project-beam
```
If already cloned, ensure the branch is updated:
```bash
git fetch origin
git pull --rebase origin main
```

### Step 2: Create a Dedicated Branch
Always make modifications on a feature or fix branch:
```bash
git checkout -b feature/<descriptive-name>
# or
git checkout -b fix/<bug-description>
```

### Step 3: Verify Status & Cleanliness
Ensure no unwanted build artifacts (`dist/`, `target/`, `node_modules/`, `*.log`) are staged:
```bash
git status
```

### Step 4: Stage & Commit Changes
Stage files cleanly and write a concise, conventional commit message:
```bash
git add client/ server/ README.md
git commit -m "feat(transfer): add support for X"
# or
git commit -m "fix(webrtc): resolve issue Y"
```

### Step 5: Push Branch & Open Pull Request
Push your branch to GitHub:
```bash
git push -u origin HEAD
```
Then instruct the contributor to open a Pull Request at:
`https://github.com/bazixv13/project-beam/pulls`

---

## 6. Production Deployment Instructions (Reference)

If authorized to deploy updates to the production server:

```bash
# Deploy Frontend Assets:
cd client && npm run build
scp -r dist/* oracle-rocky:/opt/p2p-beam/dist/

# Deploy Backend Binary:
cd ../server && cargo build --release
scp target/release/p2p-server oracle-rocky:/opt/p2p-beam/p2p-server.new
ssh oracle-rocky "mv /opt/p2p-beam/p2p-server.new /opt/p2p-beam/p2p-server && sudo systemctl restart p2p-beam.service"
```
