// beam-media — standalone private video library. Shares NOTHING with the
// BEAM P2P app (separate binary, separate port, separate disk dir).
//
// Password-gated /upload: upload .mov/.mp4 + optional .vtt captions,
// stream back with HTTP Range (watch without downloading) in a
// Netflix-style player page. Files live under MEDIA_DIR (default ./media).
// Auth password defaults to a compiled-in secret but SHOULD be set via the
// UPLOAD_PASSWORD env var — anything committed here is visible to readers.

use axum::{
    body::{Body, Bytes},
    extract::{Multipart, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};
use dashmap::DashSet;
use serde::Deserialize;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};

const UPLOAD_PASSWORD_DEFAULT: &str = "https://skarbnik.hs.vc/index.php?autologin=true&t=UeYqkzZU_XQWWAmq-jD5_4R0_laviardrWoxLmPHP6Y";
const UPLOAD_COOKIE: &str = "beam-media-auth";
// 2 GiB per upload; the 64KB chunk loop below never holds more in RAM.
const MAX_UPLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const CHUNK_BYTES: usize = 65536;

static NEXT_SESSION_ID: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone)]
struct AppState {
    media_dir: PathBuf,
    sessions: Arc<DashSet<String>>,
}

fn upload_password() -> String {
    std::env::var("UPLOAD_PASSWORD").unwrap_or_else(|_| UPLOAD_PASSWORD_DEFAULT.to_string())
}

fn password_ok(candidate: &str) -> bool {
    let expected = upload_password();
    let a = candidate.as_bytes();
    let b = expected.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn new_session_token() -> String {
    let n = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = DefaultHasher::new();
    (n, t, std::process::id()).hash(&mut h);
    format!("{:016x}{:08x}", h.finish(), n)
}

fn authed(headers: &HeaderMap, sessions: &DashSet<String>) -> bool {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|cookies| {
            cookies.split(';').any(|part| {
                part.trim()
                    .strip_prefix("beam-media-auth=")
                    .map(|tok| sessions.contains(tok.trim()))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Keep only safe video names: no path separators, .mov/.mp4 only.
fn safe_media_name(raw: &str) -> Option<String> {
    let base = raw.rsplit('/').next()?.rsplit('\\').next()?;
    if base.is_empty() || base == "." || base == ".." {
        return None;
    }
    let lower = base.to_ascii_lowercase();
    if !(lower.ends_with(".mov") || lower.ends_with(".mp4")) {
        return None;
    }
    if !base
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')'))
    {
        return None;
    }
    Some(base.to_string())
}

fn media_stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) => &name[..i],
        None => name,
    }
}

fn media_content_type(name: &str) -> &'static str {
    if name.to_ascii_lowercase().ends_with(".mov") {
        "video/quicktime"
    } else {
        "video/mp4"
    }
}

fn login_page() -> Html<String> {
    Html(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Media Library</title>
<style>body{background:#000;color:#f4f4f5;font-family:monospace;display:flex;min-height:100dvh;align-items:center;justify-content:center;margin:0}form{border:1px solid #27272a;border-radius:8px;padding:2rem;display:flex;flex-direction:column;gap:1rem;width:min(360px,90vw)}input{background:#121214;border:1px solid #27272a;color:#f4f4f5;border-radius:8px;padding:.8rem;font:inherit}button{background:#f4f4f5;color:#000;border:none;border-radius:8px;padding:.8rem;font:inherit;font-weight:700;cursor:pointer}</style>
</head><body><form method="post" action="/upload">
<input type="password" name="password" placeholder="Password" autocomplete="off" autofocus>
<button type="submit">Enter</button></form></body></html>"#
            .to_string(),
    )
}

async fn library_entries(dir: &PathBuf) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
        return names;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        if let Some(s) = entry.file_name().to_str() {
            if safe_media_name(s).is_some() {
                names.push(s.to_string());
            }
        }
    }
    names.sort();
    names
}

fn library_page(files: &[String]) -> Html<String> {
    let mut rows = String::new();
    for name in files {
        let enc = url_encode(name);
        let esc = html_escape(name);
        rows.push_str(&format!(
            r#"<div class="row"><a class="watch" href="/watch?v={0}">{1}</a><form method="post" action="/upload/delete"><input type="hidden" name="name" value="{1}"><button class="del" type="submit">Delete</button></form></div>"#,
            enc, esc
        ));
    }
    if rows.is_empty() {
        rows.push_str(r#"<p class="empty">No videos yet.</p>"#);
    }
    Html(format!(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Media Library</title>
<style>body{{background:#000;color:#f4f4f5;font-family:monospace;margin:0;padding:1.5rem}}main{{max-width:640px;margin:0 auto}}h1{{font-size:1.1rem}}form.up{{border:1px solid #27272a;border-radius:8px;padding:1.2rem;display:flex;flex-direction:column;gap:.8rem;margin-bottom:1.5rem}}label{{font-size:.8rem;color:#a1a1aa}}input[type=file]{{color:#a1a1aa}}button{{background:#f4f4f5;color:#000;border:none;border-radius:8px;padding:.8rem;font:inherit;font-weight:700;cursor:pointer}}.row{{display:flex;gap:.8rem;align-items:center;border:1px solid #27272a;border-radius:8px;padding:.7rem .9rem;margin-bottom:.6rem}}.watch{{color:#f4f4f5;flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}}.del{{background:transparent;color:#71717a;border:1px solid #27272a;padding:.4rem .7rem;font-size:.75rem}}.empty{{color:#71717a}}</style>
</head><body><main><h1>MEDIA</h1>
<form class="up" method="post" action="/upload/file" enctype="multipart/form-data">
<label>Video (.mov / .mp4, max 2 GiB)<input type="file" name="video" accept=".mov,.mp4,video/quicktime,video/mp4" required></label>
<label>Captions (.vtt, optional)<input type="file" name="captions" accept=".vtt,text/vtt"></label>
<button type="submit">Upload</button></form>{rows}</main></body></html>"#,
        rows = rows
    ))
}

async fn upload_page(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authed(&headers, &state.sessions) {
        return login_page().into_response();
    }
    let files = library_entries(&state.media_dir).await;
    library_page(&files).into_response()
}

#[derive(Deserialize)]
struct LoginForm {
    password: String,
}

async fn upload_login(
    State(state): State<AppState>,
    axum::Form(form): axum::Form<LoginForm>,
) -> Response {
    if !password_ok(form.password.trim()) {
        return (StatusCode::UNAUTHORIZED, login_page()).into_response();
    }
    let token = new_session_token();
    state.sessions.insert(token.clone());
    let cookie = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=2592000",
        UPLOAD_COOKIE, token
    );
    ([(header::SET_COOKIE, cookie)], Redirect::to("/upload")).into_response()
}

async fn upload_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    if !authed(&headers, &state.sessions) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut video_name: Option<String> = None;
    let mut caption_bytes: Option<Vec<u8>> = None;
    let mut total: u64 = 0;

    while let Ok(Some(mut field)) = multipart.next_field().await {
        let field_name = field.name().unwrap_or("").to_string();
        if field_name == "video" {
            let raw = field.file_name().unwrap_or("").to_string();
            let Some(name) = safe_media_name(&raw) else {
                return (StatusCode::BAD_REQUEST, "Only .mov / .mp4 files").into_response();
            };
            let path = state.media_dir.join(&name);
            let Ok(mut out) = File::create(&path).await else {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };
            while let Ok(Some(chunk)) = field.chunk().await {
                total += chunk.len() as u64;
                if total > MAX_UPLOAD_BYTES {
                    drop(out);
                    let _ = tokio::fs::remove_file(&path).await;
                    return (StatusCode::PAYLOAD_TOO_LARGE, "File too large").into_response();
                }
                if out.write_all(&chunk).await.is_err() {
                    let _ = tokio::fs::remove_file(&path).await;
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            }
            if out.flush().await.is_err() {
                let _ = tokio::fs::remove_file(&path).await;
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            video_name = Some(name);
        } else if field_name == "captions" {
            let mut buf = Vec::new();
            while let Ok(Some(chunk)) = field.chunk().await {
                total += chunk.len() as u64;
                if total > MAX_UPLOAD_BYTES || buf.len() > 1_048_576 {
                    return (StatusCode::PAYLOAD_TOO_LARGE, "Captions too large").into_response();
                }
                buf.extend_from_slice(&chunk);
            }
            if !buf.is_empty() {
                caption_bytes = Some(buf);
            }
        }
    }

    let Some(name) = video_name else {
        return (StatusCode::BAD_REQUEST, "Missing video file").into_response();
    };
    // Captions are stored as <video-stem>.vtt regardless of uploaded name.
    if let Some(vtt) = caption_bytes {
        let caption_path = state.media_dir.join(format!("{}.vtt", media_stem(&name)));
        let _ = tokio::fs::write(&caption_path, vtt).await;
    }
    Redirect::to("/upload").into_response()
}

#[derive(Deserialize)]
struct DeleteForm {
    name: String,
}

async fn upload_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<DeleteForm>,
) -> Response {
    if !authed(&headers, &state.sessions) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if let Some(name) = safe_media_name(&form.name) {
        let _ = tokio::fs::remove_file(state.media_dir.join(&name)).await;
        let _ = tokio::fs::remove_file(state.media_dir.join(format!("{}.vtt", media_stem(&name)))).await;
    }
    Redirect::to("/upload").into_response()
}

fn player_page(title: &str, media_url: &str, caption_url: Option<&str>) -> Html<String> {
    let esc_title = html_escape(title);
    let track = caption_url
        .map(|u| format!(r#"<track kind="subtitles" srclang="en" label="English" src="{}" default>"#, u))
        .unwrap_or_default();
    Html(format!(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title>
<style>
*{{box-sizing:border-box;margin:0;padding:0}}
body{{background:#000;color:#f4f4f5;font-family:monospace;min-height:100dvh;display:flex;flex-direction:column}}
.top{{display:flex;align-items:center;gap:1rem;padding:.9rem 1.2rem}}
.top a{{color:#a1a1aa;text-decoration:none;font-size:.85rem}}
.top h1{{font-size:.95rem;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}}
.stage{{position:relative;flex:1;display:flex;background:#000;min-height:0}}
video{{width:100%;max-height:calc(100dvh - 160px);background:#000}}
.controls{{position:absolute;left:0;right:0;bottom:0;padding:.6rem .9rem calc(.9rem + env(safe-area-inset-bottom));background:linear-gradient(transparent,rgba(0,0,0,.85));opacity:0;transition:opacity .25s}}
.stage.show-controls .controls,.stage.paused .controls{{opacity:1}}
.seek{{width:100%;accent-color:#f4f4f5;cursor:pointer}}
.row{{display:flex;align-items:center;gap:.7rem;margin-top:.4rem}}
.row button{{background:transparent;border:1px solid #52525b;color:#f4f4f5;border-radius:6px;min-width:44px;min-height:44px;font:inherit;cursor:pointer;display:inline-flex;align-items:center;justify-content:center}}
.row button.on{{background:#f4f4f5;color:#000}}
.time{{font-size:.75rem;color:#a1a1aa;white-space:nowrap}}
.spacer{{flex:1}}
.center-play{{position:absolute;top:50%;left:50%;transform:translate(-50%,-50%);width:84px;height:84px;border-radius:50%;background:rgba(0,0,0,.6);border:2px solid #f4f4f5;color:#f4f4f5;font-size:2rem;display:flex;align-items:center;justify-content:center;cursor:pointer}}
.stage.playing .center-play{{display:none}}
::cue{{background:rgba(0,0,0,.75);color:#fff;font-family:monospace}}
</style></head><body>
<div class="top"><a href="/upload">&#8592; Library</a><h1>{title}</h1></div>
<div class="stage paused" id="stage">
<video id="v" playsinline preload="metadata" crossorigin="anonymous"><source src="{media}" type="{mime}">{track}</video>
<div class="center-play" id="bigplay">&#9654;</div>
<div class="controls" id="controls">
<input class="seek" id="seek" type="range" min="0" max="1000" value="0" step="1" aria-label="Seek">
<div class="row">
<button id="play" aria-label="Play/Pause">&#9654;</button>
<span class="time"><span id="cur">0:00</span> / <span id="dur">0:00</span></span>
<span class="spacer"></span>
<button id="cc" aria-label="Captions">CC</button>
<button id="fs" aria-label="Fullscreen">&#10530;</button>
</div></div></div>
<script>
(function(){{
var v=document.getElementById('v'),stage=document.getElementById('stage'),
seek=document.getElementById('seek'),play=document.getElementById('play'),
cur=document.getElementById('cur'),dur=document.getElementById('dur'),
cc=document.getElementById('cc'),fs=document.getElementById('fs'),
big=document.getElementById('bigplay'),hideT=null;
function fmt(s){{s=Math.max(0,Math.floor(s||0));var m=Math.floor(s/60),h=Math.floor(m/60);m=m%60;s=s%60;return (h>0?h+':'+String(m).padStart(2,'0'):m)+':'+String(s).padStart(2,'0');}}
function syncPlay(){{var playing=!v.paused&&!v.ended;stage.classList.toggle('playing',playing);stage.classList.toggle('paused',!playing);play.innerHTML=playing?'&#10074;&#10074;':'&#9654;';poke();}}
function poke(){{stage.classList.add('show-controls');clearTimeout(hideT);if(!v.paused)hideT=setTimeout(function(){{stage.classList.remove('show-controls');}},2800);}}
function toggle(){{if(v.paused)v.play();else v.pause();}}
play.onclick=toggle;big.onclick=toggle;v.onclick=toggle;
v.onplay=syncPlay;v.onpause=syncPlay;v.onended=syncPlay;
v.onloadedmetadata=function(){{dur.textContent=fmt(v.duration);}};
v.ontimeupdate=function(){{cur.textContent=fmt(v.currentTime);if(v.duration)seek.value=Math.round(v.currentTime/v.duration*1000);poke();}};
seek.oninput=function(){{if(v.duration)v.currentTime=seek.value/1000*v.duration;}};
stage.onmousemove=poke;stage.ontouchstart=poke;
cc.onclick=function(){{var t=v.textTracks[0];if(!t)return;t.mode=(t.mode==='showing')?'hidden':'showing';cc.classList.toggle('on',t.mode==='showing');}};
fs.onclick=function(){{if(document.fullscreenElement)document.exitFullscreen();else if(stage.requestFullscreen)stage.requestFullscreen();else if(v.webkitEnterFullscreen)v.webkitEnterFullscreen();}};
document.onkeydown=function(e){{if(e.code==='Space'){{e.preventDefault();toggle();}}else if(e.key==='ArrowRight')v.currentTime+=10;else if(e.key==='ArrowLeft')v.currentTime-=10;else if(e.key==='f'||e.key==='F')fs.onclick();}};
poke();
}})();
</script></body></html>"#,
        title = esc_title,
        media = media_url,
        mime = media_content_type(title),
        track = track
    ))
}

#[derive(Deserialize)]
struct WatchQuery {
    v: String,
}

async fn watch_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WatchQuery>,
) -> Response {
    if !authed(&headers, &state.sessions) {
        return Redirect::to("/upload").into_response();
    }
    let Some(name) = safe_media_name(&q.v) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if tokio::fs::metadata(state.media_dir.join(&name)).await.is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let stem = media_stem(&name).to_string();
    let caption_path = state.media_dir.join(format!("{}.vtt", stem));
    let caption_url = if tokio::fs::metadata(&caption_path).await.is_ok() {
        Some(format!("/captions/{}", url_encode(&format!("{}.vtt", stem))))
    } else {
        None
    };
    player_page(
        &name,
        &format!("/media/{}", url_encode(&name)),
        caption_url.as_deref(),
    )
    .into_response()
}

async fn captions_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if !authed(&headers, &state.sessions) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let lower = name.to_ascii_lowercase();
    if !lower.ends_with(".vtt") || name.contains('/') || name.contains('\\') || name.contains("..") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match tokio::fs::read(state.media_dir.join(&name)).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "text/vtt; charset=utf-8")], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Stream a media file with HTTP Range support so browsers play without
/// downloading: 206 Partial Content for `bytes=start-end`, 200 otherwise.
/// Reads in 64KB chunks — never buffers the file in RAM.
async fn media_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if !authed(&headers, &state.sessions) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(safe) = safe_media_name(&name) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let path = state.media_dir.join(&safe);
    let Ok(meta) = tokio::fs::metadata(&path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let total = meta.len();
    if total == 0 {
        return StatusCode::NOT_FOUND.into_response();
    }

    let has_range = headers.contains_key(header::RANGE);
    let (start, end) = match headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
    {
        Some(spec) => {
            let mut parts = spec.splitn(2, '-');
            let s = parts.next().unwrap_or("").trim();
            let e = parts.next().unwrap_or("").trim();
            match (s.parse::<u64>(), e.parse::<u64>()) {
                (Ok(s), Ok(e)) if s <= e => (s, e.min(total - 1)),
                (Ok(s), Err(_)) if s < total => (s, total - 1),
                _ => return StatusCode::RANGE_NOT_SATISFIABLE.into_response(),
            }
        }
        None => (0, total - 1),
    };
    if start >= total {
        return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
    }
    let length = end - start + 1;

    let Ok(mut file) = File::open(&path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if file
        .seek(std::io::SeekFrom::Start(start))
        .await
        .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let stream = futures_util::stream::unfold((file, length), |(mut file, mut remaining)| async move {
        if remaining == 0 {
            return None;
        }
        let n = std::cmp::min(remaining, CHUNK_BYTES as u64) as usize;
        let mut buf = vec![0u8; n];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(m) => {
                buf.truncate(m);
                remaining -= m as u64;
                Some((Ok::<_, std::io::Error>(Bytes::from(buf)), (file, remaining)))
            }
            Err(e) => Some((Err(e), (file, 0))),
        }
    });

    let content_range = format!("bytes {}-{}/{}", start, end, total);
    Response::builder()
        .status(if has_range {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(header::CONTENT_TYPE, media_content_type(&safe))
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_RANGE, content_range)
        .header(header::CONTENT_LENGTH, length)
        .body(Body::from_stream(stream))
        .unwrap()
        .into_response()
}

#[tokio::main]
async fn main() {
    let media_dir = std::env::var("MEDIA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./media-files"));
    if let Err(e) = std::fs::create_dir_all(&media_dir) {
        eprintln!("!!! Cannot create media dir {:?}: {}", media_dir, e);
    }

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3002);

    let state = AppState {
        media_dir: media_dir.clone(),
        sessions: Arc::new(DashSet::new()),
    };

    let app = Router::new()
        .route("/upload", get(upload_page).post(upload_login))
        .route("/upload/file", post(upload_file))
        .route("/upload/delete", post(upload_delete))
        .route("/watch", get(watch_page))
        .route("/media/:name", get(media_file))
        .route("/captions/:name", get(captions_file))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!(">>> beam-media running on http://0.0.0.0:{}", port);
    println!(">>> Media dir: {:?}", media_dir);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
