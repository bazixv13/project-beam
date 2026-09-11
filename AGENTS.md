# AGENTS.md — AI Instructions for Project BEAM

> **Notice to AI Agents:** You are assisting a contributor working on **Project BEAM** (an ultra-lean, high-throughput WebRTC & native Rust file transfer engine). Read this document carefully before proposing, modifying, or testing any code.
> 
> **Repository:** `https://github.com/bazixv13/project-beam`
> **Branches (see `introduce.md` — it is authoritative on workflow):**
> - `develop` — agents + contributor push here. Auto-deploys to dev: `https://filetrans.duckdns.org/`
> - `main` — **owner only (protected)**. Auto-deploys to prod: `https://beam.hs.vc/`
> **Agents never push to `main`.**

---

## 1. Project Mission & Constraints

- **Objective:** Maximum throughput, zero-bloat file transfer between browsers with 0ms room connection latency and high fault-tolerance.
- **Server Environment:** Oracle Cloud Rocky Linux 9 instance (`1 GB RAM, 1 vCPU core`).
- **Critical Resource Constraint:** Memory is severely constrained on the server. The backend server must remain minimal (~1.5 MB RAM footprint) and never buffer entire files in memory. All streaming must be zero-copy or chunk-streamed in RAM.
- **Continuous Deployment:** Pushes to `develop` trigger `deploy.yml` (dev at `https://filetrans.duckdns.org/`); pushes to `main` (owner only) trigger `deploy-prod.yml` (prod at `https://beam.hs.vc/`). Both build frontend + Rust binary, deploy over SSH, and verify HTTP 200. Manual deployment steps are not required.

---

## 2. Codebase Architecture

```
project-beam/
├── .github/workflows/
│   ├── deploy.yml       # Dev CI/CD: push to `develop` → filetrans.duckdns.org
│   └── deploy-prod.yml  # Prod CI/CD: push to `main` (owner) → beam.hs.vc
├── client/              # React 19 + Vite Frontend (installable PWA)
│   ├── public/
│   │   ├── manifest.webmanifest # PWA manifest: icons, share_target (Android share sheet)
│   │   ├── sw.js        # Minimal SW: share-target POST → IndexedDB handoff, no precache
│   │   └── icon-*.png / mono-512.png / favicon.svg # Launcher, maskable, monochrome, tab icons
│   ├── src/
│   │   ├── App.jsx      # UI layout, room orchestration, theme, transfer strips
│   │   ├── main.jsx     # Entry: SW registration
│   │   ├── webrtc.js    # Core transfer engine: WebRTC DataChannel + WS relay fallback
│   │   ├── index.css    # High-contrast brutalist monochrome styling
│   │   ├── QRScanner.jsx # Camera-based QR code reader
│   │   └── QRCodeDisplay.jsx # SVG QR generator
│   ├── package.json
│   └── vite.config.js
└── server/              # Native Rust Signaling & Binary Relay Server
    ├── src/
    │   └── main.rs      # Axum server: room registry, WS relay (bounded), static + share fallback
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
2. **Departure Notification (Delayed, Grace-Based):**
   - If a peer disconnects, navigates away, or closes the socket, the slot is freed immediately but the remaining peer is notified only after a grace period (5s normal, 150s while the peer announced an open file picker), via `{"type": "user-left", "sender": id}`. The notice is suppressed if the peer rejoins in time. Never notify instantly — phones suspend sockets for seconds and instant notices cause false disconnect flicker.
3. **Binary Relay Fallback (Bounded + Backpressure, Never Drop):**
   - Relay binary chunks across BOUNDED per-peer channels (`PEER_CHANNEL_CAPACITY = 64`, ~4MB worst case). A full queue must exert backpressure on the sender's read loop — never `try_send`-and-drop relay chunks (no retransmission exists) and never restore unbounded channels (OOM risk on the 1GB box). Only pings may be best-effort skipped on a full queue.

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
# Or test full release binary build:
cargo build --release
```

---

## 5. Directions for AI Agents: How to Contribute & Push Changes

When you (the AI agent) have finished implementing and verifying code changes for the contributor, follow the `develop`-only workflow (`introduce.md` is authoritative — agents never touch `main`):

### Step 1: Sync Develop
```bash
git fetch origin
git checkout develop
git pull --rebase origin develop
```

### Step 2: Work Directly on Develop (or a Short-Lived Fix Branch)
Small changes may go straight on `develop`. Larger work:
```bash
git checkout -b fix/<bug-description>
# ... implement, verify, then merge back into develop before pushing
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

### Step 5: Push to Develop (Deploys to Dev Automatically)
```bash
git push origin develop
```

Pushing to `develop` triggers `deploy.yml`, which builds, deploys to `https://filetrans.duckdns.org/`, and verifies HTTP 200. The owner promotes `develop` → `main` for production.

### Never Do These
- **Never push to `main`** — it is owner-only and deploys to production.
- **Never use `git reset --hard`** — it rewrites shared history. To undo, use `git revert <hash>` instead.
