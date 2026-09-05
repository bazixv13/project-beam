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
git fetch origin && git checkout develop && git pull --rebase origin develop
```

> ⚠️ **Always work on the `develop` branch. Never push directly to `main`.**

---

## Current Version

```
1.3.4
```

**Agents: whenever you make a meaningful change, bump this version** (`MAJOR.MINOR.PATCH`) **and update all three version references:**
- `APP_VERSION` constant in `client/src/App.jsx`
- CSS comment on line ~113 of `client/src/index.css`
- CSS `content` string on line ~120 of `client/src/index.css`

---

## What This Project Is
BEAM is a browser-based peer-to-peer file transfer app. Two browsers connect via a 2-character room code and transfer files directly (WebRTC) or through a WebSocket relay if firewalls block direct connections.

- **Frontend:** React 19 + Vite — `client/`
- **Backend:** Native Rust (Axum) WebSocket signaling server — `server/`
- **Dev server (live after every push to `develop`):** https://filetrans.duckdns.org/
- **Production server (live after owner merges `develop` → `main`):** https://beam.hs.vc/

---

## Branch Structure

| Branch | Who can push | Auto-deploys to |
|--------|-------------|-----------------|
| `develop` | You + contributor | https://filetrans.duckdns.org/ |
| `main` | **Owner only** (protected) | https://beam.hs.vc/ |

**As an agent, you only ever push to `develop`.** The owner decides when to promote `develop` → `main` for production.

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
git push origin develop
```

**That's all.** Pushing to `develop` automatically triggers a GitHub Actions pipeline that:
1. Builds the frontend and Rust binary in CI
2. Deploys them to the server over SSH
3. Restarts the service
4. Verifies https://filetrans.duckdns.org/ returns HTTP 200

Changes will be live at https://filetrans.duckdns.org/ within ~90 seconds of pushing.

---

## How Owner Promotes develop → main (Production)

The owner does this — **not the agent:**

```bash
git checkout main
git pull origin main
git merge develop --no-ff -m "release: describe what's being released"
git push origin main
# CI fires and deploys to https://beam.hs.vc/ in ~90s
```

Or via GitHub: open a Pull Request from `develop` → `main` and merge it.
`develop` is **never deleted** — it lives alongside `main` permanently.

After merging, sync `develop` back up:
```bash
git checkout develop
git merge main --ff-only
git push origin develop
```

---

## Hard Rules — Do Not Break These

1. **Never use `blockBuffer.slice()`** in `webrtc.js` — always use `new Uint8Array(blockBuffer, blockPos, chunkLen)` (zero-copy, avoids GC pressure).
2. **Never store files on the server** — the Rust server is a pure in-memory relay. No disk writes, no database.
3. **Never buffer full files in JS heap** — the receiver batches chunks into 16 MB `Blob` arrays that flush to disk. Do not accumulate into a single growing array.
4. **Do not change the WebRTC upgrade logic** unless you fully understand it — the connection only upgrades to direct P2P after ICE state is verified (`connected`/`completed`) AND `dataChannel.readyState === 'open'`. Changing this causes receiver-side flicker and null pointer errors.
5. **Do not add new npm dependencies** without a good reason — keep the bundle lean.
6. **Do not add Rust crate dependencies** without a good reason — the server must stay under ~2 MB RAM at idle.
7. **Never push to `main`** — agents only push to `develop`.
8. **Never use `git reset --hard`** — this rewrites shared history and will break the collaborator's local branch. To undo a commit, always use `git revert <hash>` instead.

---

## Design Constraints

- **Monochrome brutalist** — no colors other than black/white and the CSS variables already defined. No gradients, no shadows, no rounded decorative elements.
- **Mobile-first** — all tap targets must be 48px or taller.
- **No marketing text** — zero taglines, slogans, or AI-generated fluff.
