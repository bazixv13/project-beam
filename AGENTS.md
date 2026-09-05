# Project BEAM — Agent & Developer Handbook

This handbook provides critical architectural context, workspace rules, build/deploy instructions, and step-by-step git directions for developers and AI agents working on **BEAM**.

---

## 1. System & Security Profile

- **Development Host:** Linux (Fedora), User: `kali`, Default Shell: `fish` (or `bash`).
- **Workspace Location:** `/home/kali/Documents/project-beam` (symlinked as `/home/kali/Documents/project beam`).
- **Target Remote Host:** Oracle Cloud Rocky Linux 9 instance (`ssh oracle-rocky`), 1 GB RAM, 1 vCPU core.
- **Domains:** `https://beam.hs.vc/`, `https://filetrans.duckdns.org/` (Public IP: `92.5.26.201`).

### Privileged Command Execution (Sudo Rules)
When executing any command requiring elevated privileges (`root`) on the local development machine:
1. **Write Explanation:** Write a single human-readable sentence explaining the command to: `/home/kali/.agents/sudo-reason.txt`
2. **Execute Command:** Run using the graphical password prompt:
   ```bash
   env SUDO_ASKPASS=/home/kali/.agents/my-askpass sudo -A <command>
   ```
3. **Constraints:** Never prompt interactively for passwords in terminal, never ask the user to type passwords in chat, and never store passwords in plaintext.
4. **System Tweaks:** Obtain explicit user consent before adjusting global system settings (e.g. `/etc`, `/usr`, `systemctl`).

---

## 2. Architecture & Components

```
project-beam/
├── client/              # React 19 + Vite Frontend
│   ├── src/
│   │   ├── App.jsx      # Main application view, room management, theme, progress UI
│   │   ├── webrtc.js    # Transfer engine: WebRTC DataChannel + WS relay fallback
│   │   ├── index.css    # Monochrome brutalist single-surface design
│   │   ├── QRScanner.jsx # Camera-based QR code room reader
│   │   └── QRCodeDisplay.jsx # Fast SVG room link QR display
│   ├── package.json
│   └── vite.config.js
├── server/              # Native Rust Signaling & Binary Relay Server
│   ├── src/
│   │   └── main.rs      # Axum WebSocket server, room registry, peer-departure broadcast
│   ├── Cargo.toml
│   └── Cargo.lock
├── .gitignore           # Git ignore rules for node_modules, target, dist, env
├── README.md            # Public project overview and quickstart
└── AGENTS.md            # This agent and developer handbook
```

### Backend (Native Rust Server)
- **Source:** `server/` (package: `p2p-server`)
- **Remote Binary:** `/opt/p2p-beam/p2p-server`
- **Listening On:** `127.0.0.1:3001`
- **Systemd Unit:** `/etc/systemd/system/p2p-beam.service` (`MemoryMax=32M`, `CPUQuota=100%`)
- **Resource Footprint:** ~1.5 MB RAM, 0.0% CPU at idle.
- **Role:** Handles 2-character rooms (`[0-9A-Z]{2}`), relays WebRTC signaling (offers, answers, ICE candidates), broadcasts peer departures (`user-left`), and provides zero-copy binary chunk streaming fallback when direct P2P is blocked.

### Frontend (React 19 + Vite)
- **Source:** `client/`
- **Remote Webroot:** `/opt/p2p-beam/dist/`
- **Design Philosophy:** Brutalist monochrome single-surface layout. High-contrast white/black progress bars, edge-to-edge mobile optimization, zero bloat.
- **Transfer Engine (`client/src/webrtc.js`):**
  - **Zero-Copy Uint8Array Slicing:** Uses `new Uint8Array(blockBuffer, blockPos, chunkLen)` directly into SCTP chunks (64 KB).
  - **Strict WebRTC Verification:** Upgrades to direct P2P only after ICE connection state is verified (`connected` or `completed`) and DataChannel is open.
  - **Graceful P2P-to-Relay Synchronization:** If WebRTC fails or times out (8s limit), both peers cleanly downgrade to WS relay (`p2p-downgrade` control signal) and the header badge immediately switches to `RELAY`.
  - **In-Flight DataChannel Send Fallback:** If DataChannel disconnects or errors during transmission, `sendFile` catches it, downgrades mode, and seamlessly continues sending remaining chunks over WebSocket without terminating or restarting the transfer.
  - **Resumable Transfers:** Preserves staged files across peer departures; checks byte offset on reconnect and resumes seamlessly.
  - **Zero-Flicker Liveness:** Heartbeat timeout relaxed to 12s, refreshed by any incoming binary chunks or control messages to prevent false peer disconnection cycles.

### Reverse Proxy & SSL (Caddy)
- **Config:** `/etc/caddy/Caddyfile` on `oracle-rocky`
- **Routing:** Reverse-proxies `beam.hs.vc` and `filetrans.duckdns.org` to `127.0.0.1:3001`.
- **Certificates:** Automated Let's Encrypt TLS with HTTP/2 & HTTP/3.

---

## 3. Local Development Workflows

### Running the Frontend
```bash
cd client
npm install
npm run dev
```

### Running the Rust Signaling Server
```bash
cd server
cargo run
```

### Building the Project
```bash
# Build frontend bundle (creates client/dist/)
cd client
npm run build

# Build release backend binary (creates server/target/release/p2p-server)
cd server
cargo build --release
```

---

## 4. Production Deployment to Oracle Cloud

### Deploying Frontend Changes
```bash
# 1. Build Vite production bundle
cd client && npm run build

# 2. Upload assets to remote server
scp -r dist/* oracle-rocky:/opt/p2p-beam/dist/
```

### Deploying Backend Changes
```bash
# 1. Build optimized release binary
cd server && cargo build --release

# 2. Upload binary to server and restart systemd service
scp target/release/p2p-server oracle-rocky:/opt/p2p-beam/p2p-server.new
ssh oracle-rocky "mv /opt/p2p-beam/p2p-server.new /opt/p2p-beam/p2p-server && sudo systemctl restart p2p-beam.service"

# 3. Check service status
ssh oracle-rocky "systemctl status p2p-beam --no-pager"
```

---

## 5. Git Collaboration & Directions to Push After Changes

### Initial Setup (Connect to Remote Repository)
If pushing to GitHub, GitLab, or a shared git remote:

1. **Configure Git Identity (if not already set globally):**
   ```bash
   git config user.name "Your Name"
   git config user.email "your.email@example.com"
   ```

2. **Add Remote Origin:**
   ```bash
   # Using SSH (recommended):
   git remote add origin git@github.com:<username>/<repo-name>.git

   # Or using HTTPS:
   git remote add origin https://github.com/<username>/<repo-name>.git
   ```

3. **Verify Remote Configuration:**
   ```bash
   git remote -v
   ```

### Daily Development & Pushing Changes

Follow this standard workflow whenever making modifications:

1. **Check Status of Working Tree:**
   ```bash
   git status
   ```
   Ensure untracked or unwanted files (like `node_modules`, `target`, `dist`) are ignored by `.gitignore`.

2. **Create a Feature Branch (for collaborative work):**
   ```bash
   git checkout -b feature/your-feature-name
   ```

3. **Stage Changes:**
   ```bash
   # Stage all modified and new files tracked by git:
   git add .

   # Or stage specific files:
   git add client/src/webrtc.js client/src/App.jsx
   ```

4. **Commit Changes with a Clear Message:**
   ```bash
   git commit -m "fix(webrtc): synchronize p2p to relay fallback and eliminate receiver flicker"
   ```

5. **Push Changes to the Remote Repository:**
   ```bash
   # Push current branch and set upstream tracking:
   git push -u origin feature/your-feature-name

   # Or if working directly on the main branch:
   git push origin main
   ```

6. **Pulling Collaborator Changes (Keeping Up to Date):**
   ```bash
   # Fetch and rebase to keep a clean commit history:
   git fetch origin
   git pull --rebase origin main
   ```

---

## 6. Key Files Quick Reference

| File | Purpose |
|------|---------|
| [client/src/webrtc.js](file:///home/kali/Documents/project-beam/client/src/webrtc.js) | WebRTC connection lifecycle, DataChannel chunking, WS fallback, liveness, pause/resume |
| [client/src/App.jsx](file:///home/kali/Documents/project-beam/client/src/App.jsx) | React application shell, room code input/display, status header, transfer strip |
| [client/src/index.css](file:///home/kali/Documents/project-beam/client/src/index.css) | Brutalist monochrome theme tokens, progress bar styling, animations |
| [server/src/main.rs](file:///home/kali/Documents/project-beam/server/src/main.rs) | Native Axum server, room manager, WebSocket framing, peer departure broadcast |
| [server/Cargo.toml](file:///home/kali/Documents/project-beam/server/Cargo.toml) | Rust dependencies and release compiler optimizations (`lto = true`, `strip = true`) |
| [.gitignore](file:///home/kali/Documents/project-beam/.gitignore) | Excludes node_modules, build targets, dist, logs, and sensitive files from git |
