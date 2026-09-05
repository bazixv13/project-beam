# BEAM — Agent Introduction & Task Brief

Send this file to any AI agent alongside your task. The agent will have everything it needs to make changes, verify them locally, and push to the live development server.

---

## Repository

```
https://github.com/bazixv13/project-beam
```

Clone it first (or pull latest if already cloned):
```bash
git clone https://github.com/bazixv13/project-beam.git
cd project-beam
# or if already cloned:
git fetch origin && git pull --rebase origin main
```

---

## What This Project Is

BEAM is a browser-based peer-to-peer file transfer app. Two browsers connect via a 2-character room code and transfer files directly (WebRTC) or through a WebSocket relay if firewalls block direct connections.

- **Frontend:** React 19 + Vite — `client/`
- **Backend:** Native Rust (Axum) WebSocket signaling server — `server/`
- **Dev server (live after every push to main):** https://filetrans.duckdns.org/

---

## Key Files

| File | What it controls |
|------|-----------------|
| `client/src/index.css` | All visual styling — colors, fonts, layout, animations |
| `client/src/App.jsx` | UI structure, room code input, connection status, transfer strips |
| `client/src/webrtc.js` | Transfer engine — WebRTC, WebSocket relay, chunking, liveness |
| `server/src/main.rs` | Rust server — room registry, WebSocket relay, peer departure alerts |

---

## Making Changes

Edit the relevant files. For visual/UI changes that's almost always `client/src/index.css` or `client/src/App.jsx`.

---

## Verify Locally Before Pushing

**Always run these checks before committing. Do not skip.**

```bash
# 1. Check the frontend builds without errors
cd client
npm install
npm run build
cd ..

# 2. Check the Rust server compiles without errors
cd server
cargo check
cd ..
```

If either command fails, fix the errors before proceeding.

---

## Commit & Push to Deploy

Once both checks pass:

```bash
git add .
git commit -m "fix: describe what changed"
git push origin main
```

**That's all.** Pushing to `main` automatically triggers a GitHub Actions pipeline that:
1. Builds the frontend and Rust binary in CI
2. Deploys them to the server over SSH
3. Restarts the service
4. Verifies the live endpoint returns HTTP 200

Changes will be live at https://filetrans.duckdns.org/ within ~90 seconds of pushing.

---

## Hard Rules — Do Not Break These

1. **Never use `blockBuffer.slice()`** in `webrtc.js` — always use `new Uint8Array(blockBuffer, blockPos, chunkLen)` (zero-copy, avoids GC pressure).
2. **Never store files on the server** — the Rust server is a pure in-memory relay. No disk writes, no database.
3. **Never buffer full files in JS heap** — the receiver batches chunks into 16 MB `Blob` arrays that flush to disk. Do not accumulate into a single growing array.
4. **Do not change the WebRTC upgrade logic** unless you fully understand it — the connection only upgrades to direct P2P after ICE state is verified (`connected`/`completed`) AND `dataChannel.readyState === 'open'`. Changing this causes receiver-side flicker and null pointer errors.
5. **Do not add new npm dependencies** without a good reason — keep the bundle lean.
6. **Do not add Rust crate dependencies** without a good reason — the server must stay under ~2 MB RAM at idle.

---

## Design Constraints

- **Monochrome brutalist** — no colors other than black/white and the CSS variables already defined. No gradients, no shadows, no rounded decorative elements.
- **Mobile-first** — all tap targets must be 48px or taller.
- **No marketing text** — zero taglines, slogans, or AI-generated fluff.
