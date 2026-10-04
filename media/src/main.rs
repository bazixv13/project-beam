// beam-media — standalone private video library. Shares NOTHING with the
// BEAM P2P app (separate binary, separate port, separate disk dir).
//
// Password-gated /upload: upload .mov/.mp4 + optional .vtt captions,
// stream back with HTTP Range (watch without downloading) in a
// Netflix-style player page. Files live under MEDIA_DIR (default ./media).
// Auth password defaults to a compiled-in secret but SHOULD be set via the
// UPLOAD_PASSWORD env var — anything committed here is visible to readers.

use axum::{
    body::{to_bytes, Body, Bytes},
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};
use dashmap::{DashMap, DashSet};
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
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};

const UPLOAD_PASSWORD_DEFAULT: &str = "admin";
const UPLOAD_COOKIE: &str = "beam-media-auth";
// 2 GiB per upload unless UPLOAD_MAX_BYTES overrides (bytes); the 64KB
// chunk loop below never holds more in RAM regardless of the cap.
const DEFAULT_MAX_UPLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
// Whole-library pool cap: uploads are refused once the media dir exceeds it.
const MAX_POOL_BYTES: u64 = 50 * 1024 * 1024 * 1024;
// Subtitle uploads via the player chooser may be up to 50 MiB.
const MAX_SUBTITLE_BYTES: usize = 50 * 1024 * 1024;

/// Video extensions playable in the browser player.
const VIDEO_EXTS: &[&str] = &["mov", "mp4", "m4v", "mkv", "webm"];
/// Everything storable. Served inline only for VIDEO_EXTS, as a forced
/// download otherwise (no inline HTML/JS/SVG rendering, no XSS surface).
const ALLOWED_EXTS: &[&str] = &[
    "mov", "mp4", "mkv", "webm", "m4v", "avi", "mpg", "mpeg", "mp3", "wav",
    "flac", "ogg", "opus", "srt", "vtt", "ass", "ssa", "txt", "pdf", "epub",
    "zip", "rar", "7z", "jpg", "jpeg", "png", "gif", "webp", "heic", "heif",
];

fn max_upload_bytes() -> u64 {
    std::env::var("UPLOAD_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_UPLOAD_BYTES)
}
const CHUNK_BYTES: usize = 65536;

static NEXT_SESSION_ID: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone)]
struct AppState {
    media_dir: PathBuf,
    sessions: Arc<DashSet<String>>,
    /// Cache filenames currently being extracted (no duplicate ffmpeg jobs).
    extracting: Arc<DashSet<String>>,
    /// Live transcoder progress: cache filename -> out_time_ms.
    progress: Arc<DashMap<String, u64>>,
    /// Total media duration per cache filename (ms), for percent calc.
    totals: Arc<DashMap<String, u64>>,
    /// In-flight chunked uploads: token -> session.
    uploads: Arc<DashMap<String, UploadSession>>,
    /// Consecutive extraction failures per cache name (stops silent loops).
    failures: Arc<DashMap<String, u32>>,
}

/// Max % of one CPU core a live box transcoding may burn (duty-cycled
/// SIGSTOP/SIGCONT + single ffmpeg thread). 0/100+ = uncapped.
/// Set CPU_LIMIT_PCT=25 in the box service unit; leave unset locally.
fn cpu_limit_pct() -> u64 {
    std::env::var("CPU_LIMIT_PCT")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(|n: u64| n.min(100))
        .unwrap_or(0)
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
    // 8 bytes of real entropy so tokens are unguessable even if the counter
    // and clock are known (falls back to pid mix on exotic platforms).
    let mut rnd = [0u8; 8];
    {
        use std::io::Read;
        if std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut rnd))
            .is_err()
        {
            let pid = std::process::id();
            rnd[..4].copy_from_slice(&pid.to_ne_bytes());
            rnd[4..].copy_from_slice(&pid.to_ne_bytes());
        }
    }
    let mut h = DefaultHasher::new();
    (n, t, std::process::id(), u64::from_ne_bytes(rnd)).hash(&mut h);
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

/// Keep only safe stored names: no path separators, allowlisted extension,
/// ASCII-only, capped length (filesystem limits + log sanity).
fn safe_file_name(raw: &str) -> Option<String> {
    let base = raw.rsplit('/').next()?.rsplit('\\').next()?;
    if base.is_empty() || base == "." || base == ".." {
        return None;
    }
    if base.len() > 200 {
        return None;
    }
    let lower = base.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    if !ALLOWED_EXTS.contains(&ext) {
        return None;
    }
    if !base
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')' | '[' | ']'))
    {
        return None;
    }
    Some(base.to_string())
}

/// Keep only safe video names: no path separators, .mov/.mp4 only.
fn safe_media_name(raw: &str) -> Option<String> {
    let name = safe_file_name(raw)?;
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    if VIDEO_EXTS.contains(&ext) {
        Some(name)
    } else {
        None
    }
}

fn is_video(name: &str) -> bool {
    safe_media_name(name).is_some()
}

/// All caption files affiliated with a video: <stem>.vtt plus
/// <stem>.<label>.vtt. Returns (label, filename), sorted.
async fn affiliated_captions(dir: &PathBuf, video: &str) -> Vec<(String, String)> {
    let stem = media_stem(video).to_string();
    let mut out = Vec::new();
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
        return out;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let Some(s) = entry.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
    let lower = s.to_ascii_lowercase();
    if !lower.ends_with(".vtt") {
        continue;
    }
    // Extraction cache (<stem>.sub<idx>.vtt) is served through /tracks
    // with proper [LANG] labels, not the filename heuristic.
    if is_cache_file(&s) {
        continue;
    }
        let base = &s[..s.len() - 4];
        if base == stem {
            out.push(("Subtitles".to_string(), s));
        } else if let Some(label) = base.strip_prefix(&format!("{}.", stem)) {
            if !label.is_empty() && !label.contains('.') && !label.contains('/') {
                out.push((label.to_string(), s));
            }
        }
    }
    out.sort();
    out
}

async fn dir_usage(dir: &PathBuf) -> u64 {
    let mut sum = 0u64;
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
        return 0;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        if let Ok(meta) = entry.metadata().await {
            sum = sum.saturating_add(meta.len());
        }
    }
    sum
}

fn media_stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) => &name[..i],
        None => name,
    }
}

fn media_ext(name: &str) -> String {
    match name.rfind('.') {
        Some(i) => name[i + 1..].to_ascii_lowercase(),
        None => String::new(),
    }
}

/// Browsers render these natively (no custom player needed): images, PDFs
/// and audio get the browser's own viewer/player; VIDEO_EXTS additionally
/// get the watch page. Everything else stays a forced download — notably
/// never inline HTML/SVG (XSS surface).
fn inline_native(name: &str) -> bool {
    matches!(
        media_ext(name).as_str(),
        "mov" | "mp4" | "m4v" | "mkv" | "webm"
            | "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif"
            | "pdf"
            | "mp3" | "wav" | "flac" | "ogg" | "opus"
    )
}

fn media_content_type(name: &str) -> &'static str {
    match media_ext(name).as_str() {
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" | "heif" => "image/heic",
        "pdf" => "application/pdf",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        _ => "video/mp4",
    }
}

fn login_page() -> Html<String> {
    Html(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Media Library</title>
<link rel="icon" type="image/svg+xml" href="/media/favicon.svg">
<style>html{background:#000;user-select:none;-webkit-user-select:none}body{background:#000;color:#f4f4f5;font-family:monospace;display:flex;min-height:100vh;min-height:100dvh;align-items:center;justify-content:center;margin:0}form{border:1px solid #27272a;border-radius:8px;padding:2rem;display:flex;flex-direction:column;gap:1rem;width:min(360px,90vw)}input{background:#121214;border:1px solid #27272a;color:#f4f4f5;border-radius:8px;padding:.8rem;font:inherit}button{background:#f4f4f5;color:#000;border:none;border-radius:8px;padding:.8rem;font:inherit;font-weight:700;cursor:pointer}</style>
</head><body><form method="post" action="/upload">
<input type="password" name="password" placeholder="Password" autocomplete="off" autofocus>
<button type="submit">Enter</button></form></body></html>"#
            .to_string(),
    )
}

async fn library_entries(dir: &PathBuf) -> Vec<(String, u64)> {
    let mut names = Vec::new();
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
        return names;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        if let Some(s) = entry.file_name().to_str() {
            // Caption sidecars + extraction cache stay hidden: they live
            // under their video (player menus, never the library).
            if s.to_ascii_lowercase().ends_with(".vtt") || is_cache_file(s) {
                continue;
            }
            if safe_file_name(s).is_some() {
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                names.push((s.to_string(), size));
            }
        }
    }
    names.sort();
    names
}

fn fmt_size(b: u64) -> String {
    if b < 1024 {
        format!("{} B", b)
    } else if b < 1024 * 1024 {
        format!("{:.1} KB", b as f64 / 1024.0)
    } else if b < 1024 * 1024 * 1024 {
        format!("{:.1} MB", b as f64 / 1048576.0)
    } else {
        format!("{:.2} GB", b as f64 / 1073741824.0)
    }
}

// Displayed in the library header on hover. Deliberately SEPARATE from the
// main BEAM app versioning (that client is v1.3.x). The number comes from
// Cargo.toml (bump it for meaning); the short git hash is baked in
// automatically at compile time by build.rs, so every deploy reports the
// exact commit it runs.
const MEDIA_VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"), "-", env!("GIT_HASH"));
// Short display string (layout-stable); the full hash rides in the tooltip.
const MEDIA_VERSION_SHORT: &str = concat!("v", env!("CARGO_PKG_VERSION"));

fn library_page(files: &[(String, u64)], pool_used: u64) -> Html<String> {
    let mut rows = String::new();
    for (name, size) in files {
        let enc = url_encode(name);
        let esc = html_escape(name);
        let fsize = fmt_size(*size);
        // Player videos get the watch page; other browser-native types
        // (images, PDFs, audio) open directly in the browser's own viewer;
        // the rest download.
        let href = if is_video(name) {
            format!("/watch?v={}", enc)
        } else if inline_native(name) {
            format!("/media/{}", enc)
        } else {
            format!("/media/{}?dl=1", enc)
        };
        rows.push_str(&format!(
            r#"<div class="row"><a class="watch" href="{0}">{1}</a><span class="fsize">{2}</span><form method="post" action="/upload/delete"><input type="hidden" name="name" value="{1}"><button class="del" type="button" onclick="return armDel(this)">Delete</button></form></div>"#,
            href, esc, fsize
        ));
    }
    if rows.is_empty() {
        rows.push_str(r#"<p class="empty">No videos yet.</p>"#);
    }
    Html(format!(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Media Library</title>
<link rel="icon" type="image/svg+xml" href="/media/favicon.svg">
<style>@import url('https://fonts.googleapis.com/css2?family=Bebas+Neue&display=swap');html{{background:#000;user-select:none;-webkit-user-select:none}}body{{background:#000;color:#f4f4f5;font-family:monospace;margin:0;padding:1.5rem;min-height:100vh;min-height:100dvh;box-sizing:border-box}}main{{max-width:640px;margin:0 auto}}h1{{font-size:1.1rem}}form.up{{border:1px solid #27272a;border-radius:8px;padding:1.2rem;display:flex;flex-direction:column;gap:.8rem;margin-bottom:1.5rem}}label{{font-size:.8rem;color:#a1a1aa}}input[type=file]{{color:#a1a1aa}}button{{background:#f4f4f5;color:#000;border:none;border-radius:8px;padding:.8rem;font:inherit;font-weight:700;cursor:pointer}}.row{{display:flex;gap:.8rem;align-items:center;border:1px solid #27272a;border-radius:8px;padding:.7rem .9rem;margin-bottom:.6rem}}.watch{{color:#f4f4f5;flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}}.del{{background:transparent;color:#71717a;border:1px solid #27272a;padding:.4rem .7rem;font-size:.75rem}}
.del.armed{{background:#f4f4f5;color:#000;border-color:#f4f4f5;font-weight:700}}.empty{{color:#71717a}}
.dz{{border:2px dashed #52525b;border-radius:12px;padding:1.6rem 1.2rem;text-align:center;margin-bottom:1rem;cursor:pointer}}
.dz.over{{border-color:#f4f4f5;background:#121214}}
.dz-t{{margin:0 0 .3rem;font-size:.95rem}}.dz-s{{margin:0;font-size:.75rem;color:#71717a}}
.qitem{{border:1px solid #27272a;border-radius:8px;padding:.7rem .9rem;margin-bottom:.6rem}}
.qname{{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:.85rem}}
.qmeta{{font-size:.75rem;color:#a1a1aa;margin:.25rem 0}}
.qbarw{{height:8px;background:#27272a;border-radius:4px;overflow:hidden;margin:.3rem 0}}
.qbar{{height:100%;width:0%;background:#f4f4f5}}
.qrow{{display:flex;gap:.4rem;margin-top:.3rem;align-items:center;flex-wrap:wrap}}
.qbtn{{background:transparent;color:#e4e4e7;border:1px solid #52525b;border-radius:6px;padding:.3rem .6rem;font-size:.75rem;font-family:inherit;cursor:pointer}}
.qsel{{background:#121214;color:#f4f4f5;border:1px solid #52525b;border-radius:6px;padding:.3rem;font-size:.75rem;font-family:inherit;max-width:100%;min-height:44px}}
.xcode{{font-size:.7rem;color:#71717a;flex-shrink:0;white-space:nowrap}}
.fsize{{font-size:.72rem;color:#71717a;white-space:nowrap;flex-shrink:0}}
.pool{{font-size:.75rem;color:#71717a;margin:.2rem 0 .3rem}}
.poolbarw{{height:4px;background:#27272a;border-radius:2px;overflow:hidden;margin-bottom:1rem}}
.poolbar{{height:100%;background:#71717a}}
.brand{{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,"Helvetica Neue",Arial,sans-serif;font-size:1.15rem;font-weight:800;letter-spacing:.08em;margin:0 0 .2rem;user-select:none;-webkit-user-select:none}}
.brand .swap{{display:inline-grid;position:relative;cursor:default}}
.brand .swap::after{{content:'{mvershort}';visibility:hidden;height:0;overflow:hidden;grid-area:1 / 1;white-space:nowrap}}
.brand #beamlogo,.brand #beamver{{grid-area:1 / 1;white-space:nowrap;transition:opacity .35s ease}}
.brand #beamver{{opacity:0;font-weight:800}}
.brand .swap.show-ver #beamlogo{{opacity:0}}
.brand .swap.show-ver #beamver{{opacity:1}}
.brand .media-sub{{font-family:'Bebas Neue','Arial Narrow',sans-serif;font-size:1rem;font-weight:400;letter-spacing:.1em;color:#71717a}}
.qbtn{{background:transparent;color:#e4e4e7;border:1px solid #52525b;border-radius:6px;padding:.3rem .6rem;font-size:.75rem;font-family:inherit;cursor:pointer;min-height:44px;min-width:44px}}
.del{{background:transparent;color:#71717a;border:1px solid #27272a;padding:.4rem .7rem;font-size:.75rem;min-height:44px;min-width:44px}}
@media (max-width:480px){{body{{padding:1rem}}.dz{{padding:1.2rem .8rem}}.row{{padding:.6rem .7rem;gap:.6rem}}.fsize{{font-size:.68rem}}}}
.uperr{{color:#f87171;font-size:.8rem;margin:0 0 .6rem}}</style>
</head><body><main><h1 class="brand"><span class="swap" id="swap" title="{mver}"><span id="beamlogo">BEAM</span><span id="beamver">{mvershort}</span></span> <span class="media-sub">MEDIA</span></h1>
<p class="pool">{pool_txt}</p><div class="poolbarw"><div class="poolbar" style="width:{pool_pct}%"></div></div>
<div class="dz" id="dz"><p class="dz-t">Drop files here or tap to browse</p><p class="dz-s">Resumable chunks · any type · .vtt attaches to a video · pool max 50 GB</p><input type="file" id="dzfile" multiple hidden></div>
<div id="queue"></div>
<p class="uperr" id="uperr" hidden></p>{rows}</main>
<script>
function armDel(btn){{if(btn.dataset.armed){{btn.closest('form').submit();return false;}}btn.dataset.armed='1';var old=btn.textContent;btn.textContent='Sure?';btn.classList.add('armed');setTimeout(function(){{delete btn.dataset.armed;btn.textContent=old;btn.classList.remove('armed');}},3000);return false;}}
(function(){{var dz=document.getElementById('dz');if(!dz)return;
var fi=document.getElementById('dzfile'),qv=document.getElementById('queue');
var CHUNK=8*1024*1024,items=[],active=null;
function mb(n){{return (n/1048576).toFixed(n<10485760?1:0)+' MB';}}
function fsz(n){{return n>=1073741824?(n/1073741824).toFixed(2)+' GB':mb(n);}}
function eta(s){{if(!isFinite(s)||s<0)return '';s=Math.round(s);if(s<60)return s+'s left';return Math.floor(s/60)+'m '+String(s%60).padStart(2,'0')+' left';}}
function ukey(it){{return 'beam-up-'+it.size+'-'+it.name;}}
function vids(){{var a=[],els=document.querySelectorAll('.row .watch');for(var i=0;i<els.length;i++)a.push(els[i].textContent);return a;}}
dz.addEventListener('click',function(){{fi.click();}});
['dragenter','dragover'].forEach(function(ev){{dz.addEventListener(ev,function(e){{e.preventDefault();dz.classList.add('over');}});}});
['dragleave','drop'].forEach(function(ev){{dz.addEventListener(ev,function(e){{e.preventDefault();dz.classList.remove('over');}});}});
dz.addEventListener('drop',function(e){{if(e.dataTransfer&&e.dataTransfer.files)addFiles(e.dataTransfer.files);}});
fi.addEventListener('change',function(){{addFiles(fi.files);fi.value='';}});
function addFiles(list){{for(var i=0;i<list.length;i++){{var f=list[i];var dup=false;for(var j=0;j<items.length;j++)if(items[j].name===f.name&&items[j].size===f.size){{dup=true;break;}}if(dup||!f.size)continue;
var it={{f:f,name:f.name,size:f.size,received:0,token:null,cap:/\.vtt$/i.test(f.name),target:null,state:'queued',err:''}};
items.push(it);render(it);
if(it.cap){{/* waits for target pick */}}else resumeCheck(it);}}
pump();}}
function render(it){{var el=document.getElementById(it.el);if(!el){{el=document.createElement('div');el.className='qitem';it.el='q'+items.indexOf(it)+'_'+Date.now();el.id=it.el;qv.appendChild(el);}}
var p=it.size?Math.round(it.received/it.size*100):0;
var h='<div class="qname">'+it.name.replace(/&/g,'&amp;').replace(/</g,'&lt;')+'</div>';
if(it.cap){{h+='<div class="qmeta">subtitle → <select class="qsel" data-q="t">'+targetOpts(it.target)+'</select></div>';}}
h+='<div class="qbarw"><div class="qbar" data-q="b" style="width:'+p+'%"></div></div>';
h+='<div class="qmeta" data-q="m">'+metaText(it,p)+'</div><div class="qrow" data-q="r">'+btns(it)+'</div>';
el.innerHTML=h;
var btns2=el.querySelectorAll('button');for(var i=0;i<btns2.length;i++)btns2[i].onclick=btnAct(it,btns2[i].getAttribute('data-a'));
var sel=el.querySelector('select');if(sel)sel.onchange=function(){{it.target=sel.value||null;if(it.target){{it.state='queued';it.err='';pump();}}render(it);}};}}
function targetOpts(cur){{var o='<option value="">pick a video…</option>';var vs=vids();for(var i=0;i<vs.length;i++){{var v=vs[i].replace(/&/g,'&amp;').replace(/</g,'&lt;');o+='<option value="'+vs[i].replace(/"/g,'&quot;')+'"'+(vs[i]===cur?' selected':'')+'>'+v+'</option>';}}return o;}}
function metaText(it,p){{if(it.state==='error')return 'Error: '+it.err;if(it.state==='done')return 'Done ✓';if(it.cap&&!it.target)return mb(it.size)+' · pick a video above to attach';var s=p+'% · '+mb(it.received)+' / '+mb(it.size);if(it.state==='active'&&it._spd)s+=' · '+mb(it._spd)+'/s '+eta(it._eta);if(it.state==='paused')s+=' · paused';return s;}}
function btns(it){{if(it.state==='done')return '';if(it.state==='active')return '<button class="qbtn" data-a="pause">Pause</button><button class="qbtn" data-a="cancel">Cancel</button>';if(it.state==='paused')return '<button class="qbtn" data-a="resume">Resume</button><button class="qbtn" data-a="cancel">Cancel</button>';if(it.state==='error')return '<button class="qbtn" data-a="retry">Retry</button><button class="qbtn" data-a="cancel">Cancel</button>';return '<button class="qbtn" data-a="cancel">Cancel</button>';}}
function btnAct(it,a){{return function(){{if(a==='pause'&&it===active){{it.state='paused';if(it.abort)it.abort.abort();render(it);active=null;pump();}}else if(a==='resume'){{it.state='queued';it.err='';render(it);pump();}}else if(a==='retry'){{it.state='queued';it.err='';render(it);pump();}}else if(a==='cancel'){{cancelItem(it);}};}};}}
function cancelItem(it){{var t=it.token;it.state='cancelled';if(it.abort)it.abort.abort();var el=document.getElementById(it.el);if(el)el.remove();items.splice(items.indexOf(it),1);try{{localStorage.removeItem(ukey(it));}}catch(_e){{}}if(t)fetch('/upload/cancel',{{method:'POST',headers:{{'Content-Type':'application/x-www-form-urlencoded'}},body:'token='+encodeURIComponent(t),credentials:'same-origin'}}).catch(function(){{}});if(active===it){{active=null;pump();}}}}
function resumeCheck(it){{var raw=null;try{{raw=localStorage.getItem(ukey(it));}}catch(_e){{}}if(!raw)return;var tk=null;try{{tk=JSON.parse(raw).token;}}catch(_e){{}}if(!tk)return;it.token=tk;
fetch('/upload/status?name='+encodeURIComponent(it.name)+'&size='+it.size,{{credentials:'same-origin'}}).then(function(r){{if(!r.ok)throw 0;return r.json();}}).then(function(j){{it.token=j.token;it.received=j.received||0;try{{localStorage.setItem(ukey(it),JSON.stringify({{token:it.token}}));}}catch(_e){{}}render(it);pump();}}).catch(function(){{it.token=null;}});}}
function pump(){{if(active)return;for(var i=0;i<items.length;i++){{var it=items[i];if(it.state==='queued'&&(!it.cap||it.target)){{active=it;run(it);return;}}}}}}
async function run(it){{it.state='active';it.err='';render(it);
try{{
if(!it.token){{var sp=new URLSearchParams();sp.append('name',it.name);sp.append('size',String(it.size));sp.append('kind',it.cap?'caption':'video');if(it.cap)sp.append('for_video',it.target);
var sr=await fetch('/upload/start',{{method:'POST',body:sp,credentials:'same-origin'}});
if(sr.status===401){{location.reload();return;}}
if(!sr.ok)throw new Error(await sr.text().then(function(t){{return t.slice(0,100);}}));
var sj=await sr.json();it.token=sj.token;it.received=sj.received||0;
try{{localStorage.setItem(ukey(it),JSON.stringify({{token:it.token}}));}}catch(_e){{}}}}
var lastT=Date.now(),lastB=it.received;it._spd=0;it._eta=0;
while(it.received<it.size){{if(it.state!=='active')return;
var end=Math.min(it.received+CHUNK,it.size);
it.abort=new AbortController();
var r=await fetch('/upload/chunk?token='+encodeURIComponent(it.token)+'&offset='+it.received,{{method:'POST',body:it.f.slice(it.received,end),signal:it.abort.signal,credentials:'same-origin'}});
if(r.status===401){{location.reload();return;}}
if(r.status===409){{var cj=await r.json();it.received=cj.received;continue;}}
if(r.status===404){{it.token=null;try{{localStorage.removeItem(ukey(it));}}catch(_e){{}}throw new Error('session lost, retry');}}
if(!r.ok)throw new Error('chunk '+r.status);
var j=await r.json();it.received=j.received;
var now=Date.now(),dt=(now-lastT)/1000;if(dt>0.5){{it._spd=(it.received-lastB)/dt;lastT=now;lastB=it.received;it._eta=it._spd>0?(it.size-it.received)/it._spd:-1;}}
render(it);}}
var fr=await fetch('/upload/finish',{{method:'POST',headers:{{'Content-Type':'application/x-www-form-urlencoded'}},body:'token='+encodeURIComponent(it.token),credentials:'same-origin'}});
if(!fr.ok)throw new Error('finish failed');
var fj=await fr.json();
try{{localStorage.removeItem(ukey(it));}}catch(_e){{}}
it.state='done';it.received=it.size;
if(!it.cap)addRow(fj.name,it.size);
render(it);
setTimeout(function(){{var el=document.getElementById(it.el);if(el)el.remove();var ix=items.indexOf(it);if(ix>=0)items.splice(ix,1);}},6000);
}}catch(e){{if(it.state==='cancelled'){{/* row gone, nothing to show */}}else if(e&&e.name==='AbortError'&&it.state==='paused'){{render(it);}}else{{it.state='error';it.err=String((e&&e.message)||e).slice(0,120);render(it);}}}}
active=null;pump();}}
// Subtle transcode hint on library rows: polls in-flight extractions and
// tags matching rows with a small gray percent. Nothing flashes, no layout
// shift — the span appears and quietly disappears when done.
function pollXcode(){{fetch('/upload/transcodes',{{credentials:'same-origin'}}).then(function(r){{return r.ok?r.json():[];}}).then(function(list){{
var rows=document.querySelectorAll('.row');
for(var i=0;i<rows.length;i++){{var a=rows[i].querySelector('.watch');if(!a)continue;
var mark=null;for(var k=0;k<list.length;k++)if(list[k].name===a.textContent){{mark=list[k];break;}}
var old=rows[i].querySelector('.xcode');
if(!mark){{if(old)old.remove();continue;}}
var txt=mark.kind==='audio'?'sound':'subs';
if(mark.progress!=null)txt+=' '+mark.progress+'%';
else txt+=' …';
if(old)old.textContent=txt;
else{{var s=document.createElement('span');s.className='xcode';s.textContent=txt;a.parentNode.insertBefore(s,a.nextSibling);}}}}}}).catch(function(){{}});}}
setInterval(pollXcode,2000);pollXcode();
// Brand hover: hold BEAM 2s → cross-fades to the service version; 4s later
// it fades back even mid-hover. Leaving early hides it IMMEDIATELY (the CSS
// transition keeps it smooth). Re-arms on mouse leave. Tap = instant reveal
// (touch has no hover).
var swapEl=document.getElementById('swap');
if(swapEl){{var hoverT=null,revertT=null,armed=true;
function showVer(){{swapEl.classList.add('show-ver');clearTimeout(revertT);revertT=setTimeout(hideVer,4000);}}
function hideVer(){{clearTimeout(hoverT);clearTimeout(revertT);swapEl.classList.remove('show-ver');}}
swapEl.addEventListener('mouseenter',function(){{if(!armed)return;clearTimeout(hoverT);hoverT=setTimeout(function(){{if(armed){{armed=false;showVer();}}}},2000);}});
swapEl.addEventListener('mouseleave',function(){{if(armed)clearTimeout(hoverT);armed=true;hideVer();}});
swapEl.addEventListener('click',function(){{clearTimeout(hoverT);armed=false;showVer();}});}}
function addRow(name,size){{var empty=document.querySelector('main .empty');if(empty)empty.remove();
var low=name.toLowerCase();
function ext(){{var i=low.lastIndexOf('.');return i>=0?low.slice(i+1):'';}}
var e=ext();
var isVid=['mov','mp4','m4v','mkv','webm'].indexOf(e)>=0;
var isNative=isVid||['jpg','jpeg','png','gif','webp','heic','heif','pdf','mp3','wav','flac','ogg','opus'].indexOf(e)>=0;
var div=document.createElement('div');div.className='row';
var a=document.createElement('a');a.className='watch';a.textContent=name;
a.href=isVid?'/watch?v='+encodeURIComponent(name):(isNative?'/media/'+encodeURIComponent(name):'/media/'+encodeURIComponent(name)+'?dl=1');
var sz=document.createElement('span');sz.className='fsize';sz.textContent=(size>=1073741824?(size/1073741824).toFixed(2)+' GB':(size/1048576).toFixed(size<10485760?1:0)+' MB');
var f=document.createElement('form');f.method='post';f.action='/upload/delete';
var h=document.createElement('input');h.type='hidden';h.name='name';h.value=name;
var b=document.createElement('button');b.className='del';b.type='button';b.textContent='Delete';
b.setAttribute('onclick','return armDel(this)');
f.appendChild(h);f.appendChild(b);div.appendChild(a);div.appendChild(sz);div.appendChild(f);
var qv2=document.getElementById('queue');qv2.parentNode.insertBefore(div,qv2.nextSibling);}}
}})();
</script></body></html>"#,
        rows = rows,
        pool_txt = format!("{} / 50 GB used", fmt_size(pool_used)),
        pool_pct = (pool_used * 100 / MAX_POOL_BYTES).min(100),
        mver = MEDIA_VERSION,
        mvershort = MEDIA_VERSION_SHORT
    ))
}

async fn upload_page(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authed(&headers, &state.sessions) {
        return login_page().into_response();
    }
    let files = library_entries(&state.media_dir).await;
    let pool_used = dir_usage(&state.media_dir).await;
    library_page(&files, pool_used).into_response()
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

// ============ RESUMABLE CHUNKED UPLOADS ============
// Single-POST uploads die with the connection on multi-GB files, so the
// client splits every file into ordered chunks:
//
//   POST /upload/start  {name, size, kind, for?, label?} -> {token, received}
//   POST /upload/chunk?token=&offset=  (octet-stream body) -> {received}
//   POST /upload/finish {token} -> {name}
//
// A dropped connection only loses the in-flight chunk: the client re-asks
// /upload/status?name=&size= (or reuses its token) and resumes at `received`.
// Chunk tmp files are hidden `.upload-<token>.part` names, published by
// atomic rename on finish. Sessions idle >30min are swept with their tmps.

/// Max bytes accepted in one chunk POST (client sends 8 MiB).
const MAX_CHUNK_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
struct UploadSession {
    /// Final filename in the media dir.
    name: String,
    /// Declared total size; finish requires received == size.
    size: u64,
    received: u64,
    tmp: PathBuf,
    /// Caption sidecar (final name derived) vs plain file.
    caption: bool,
    updated: Instant,
}

fn upload_tmp_name(token: &str) -> String {
    format!(".upload-{}.part", token)
}

#[derive(Deserialize)]
struct UploadStart {
    name: String,
    size: u64,
    /// "video" (any allowed file) or "caption" (.vtt for an existing video).
    kind: String,
    /// Caption target video (required when kind == "caption").
    #[serde(default)]
    for_video: String,
}

/// Kick the default-audio extraction for a freshly finished video, but only
/// when the container default is not browser-playable (nothing to do for
/// plain AAC/MP3). Runs through the same atomic run_extract machinery as
/// on-demand prepares.
async fn maybe_transcode_finished(state: &AppState, video: &str) {
    let path = state.media_dir.join(video);
    let (_, audios) = match probe_tracks(&path).await {
        Ok(t) => t,
        Err(_) => return,
    };
    let Some(def) = audios.iter().find(|a| a.is_default) else {
        return;
    };
    if audio_native_playable(&def.codec) {
        return;
    }
    let cache = audio_cache_name(video, def.index);
    let dst = state.media_dir.join(&cache);
    if tokio::fs::metadata(&dst).await.is_ok() || !state.extracting.insert(cache.clone()) {
        return;
    }
    let tmp = state.media_dir.join(format!("{}.part", &cache));
    let args = vec![
        "-map".to_string(),
        format!("0:{}", def.index),
        "-vn".to_string(),
        "-c:a".to_string(),
        "aac".to_string(),
        "-b:a".to_string(),
        "160k".to_string(),
    ];
    let st = state.clone();
    tokio::spawn(run_extract(path, args, "mp4", tmp, dst, cache, st));
}

async fn upload_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<UploadStart>,
) -> Response {
    if !authed(&headers, &state.sessions) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let is_caption = form.kind.as_str() == "caption";
    if form.size == 0 {
        return (StatusCode::BAD_REQUEST, "Empty file").into_response();
    }
    // Resolve + validate the final filename first: resume matches on it.
    let name = if is_caption {
        let Some(target) = safe_media_name(&form.for_video) else {
            return (StatusCode::BAD_REQUEST, "Unknown target video").into_response();
        };
        if tokio::fs::metadata(state.media_dir.join(&target)).await.is_err() {
            return (StatusCode::NOT_FOUND, "Target video not stored").into_response();
        }
        let derived = caption_file_name(&target, Some(&form.name));
        let Some(valid) = safe_file_name(&derived) else {
            return (StatusCode::BAD_REQUEST, "Bad caption name").into_response();
        };
        if form.size as usize > MAX_SUBTITLE_BYTES {
            return (StatusCode::PAYLOAD_TOO_LARGE, "Captions too large").into_response();
        }
        valid
    } else {
        if form.kind.as_str() != "video" {
            return (StatusCode::BAD_REQUEST, "Bad kind").into_response();
        }
        let Some(valid) = safe_file_name(&form.name) else {
            return (StatusCode::BAD_REQUEST, "File type not allowed").into_response();
        };
        if form.size > max_upload_bytes() {
            return (StatusCode::PAYLOAD_TOO_LARGE, "File too large").into_response();
        }
        valid
    };
    if dir_usage(&state.media_dir).await.saturating_add(form.size) > MAX_POOL_BYTES {
        return (StatusCode::PAYLOAD_TOO_LARGE, "Library full").into_response();
    }
    // Resume: same final name + size already in flight.
    for entry in state.uploads.iter() {
        let s = entry.value();
        if s.name == name && s.size == form.size {
            return axum::Json(serde_json::json!({
                "token": entry.key(),
                "received": s.received,
            }))
            .into_response();
        }
    }
    let token = new_session_token();
    let tmp = state.media_dir.join(upload_tmp_name(&token));
    // A stale tmp from a dead session (pre-restart crash) restarts clean.
    let received = match tokio::fs::metadata(&tmp).await {
        Ok(_) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            0
        }
        Err(_) => 0,
    };
    state.uploads.insert(
        token.clone(),
        UploadSession {
            name,
            size: form.size,
            received,
            tmp,
            caption: is_caption,
            updated: Instant::now(),
        },
    );
    axum::Json(serde_json::json!({ "token": token, "received": received })).into_response()
}

#[derive(Deserialize)]
struct UploadChunkQuery {
    token: String,
    offset: u64,
}

async fn upload_chunk(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<UploadChunkQuery>,
    body: Body,
) -> Response {
    if !authed(&headers, &state.sessions) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut entry = match state.uploads.get_mut(&q.token) {
        Some(e) => e,
        None => return (StatusCode::NOT_FOUND, "Unknown upload, restart it").into_response(),
    };
    if q.offset != entry.received {
        let received = entry.received;
        drop(entry);
        return (
            StatusCode::CONFLICT,
            axum::Json(serde_json::json!({ "received": received })),
        )
            .into_response();
    }
    let bytes = match to_bytes(body, MAX_CHUNK_BYTES).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::BAD_GATEWAY, "Chunk interrupted").into_response(),
    };
    if bytes.is_empty() {
        let received = entry.received;
        drop(entry);
        return axum::Json(serde_json::json!({ "received": received })).into_response();
    }
    if entry.received + bytes.len() as u64 > entry.size {
        return (StatusCode::BAD_REQUEST, "Chunk overruns declared size").into_response();
    }
    let mut out = match OpenOptions::new().create(true).append(true).open(&entry.tmp).await {
        Ok(f) => f,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if out.write_all(&bytes).await.is_err() || out.flush().await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    drop(out);
    entry.received += bytes.len() as u64;
    entry.updated = Instant::now();
    let received = entry.received;
    drop(entry);
    axum::Json(serde_json::json!({ "received": received })).into_response()
}

#[derive(Deserialize)]
struct UploadFinish {
    token: String,
}

async fn upload_finish(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<UploadFinish>,
) -> Response {
    if !authed(&headers, &state.sessions) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let session = match state.uploads.get(&form.token) {
        Some(e) => e.clone(),
        None => return (StatusCode::NOT_FOUND, "Unknown upload").into_response(),
    };
    if session.received != session.size {
        return (StatusCode::CONFLICT, "Upload incomplete").into_response();
    }
    let dst = state.media_dir.join(&session.name);
    if tokio::fs::rename(&session.tmp, &dst).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    state.uploads.remove(&form.token);
    // Fire-and-forget: undecodable default audio starts transcoding now,
    // playable files need nothing.
    if !session.caption {
        if safe_media_name(&session.name).is_some() {
            maybe_transcode_finished(&state, &session.name).await;
        }
    }
    axum::Json(serde_json::json!({ "name": session.name })).into_response()
}

async fn upload_cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<UploadFinish>,
) -> Response {
    if !authed(&headers, &state.sessions) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if let Some((_, session)) = state.uploads.remove(&form.token) {
        let _ = tokio::fs::remove_file(&session.tmp).await;
    }
    StatusCode::OK.into_response()
}

#[derive(Deserialize)]
struct UploadStatusQuery {    name: String,
    size: u64,
}

async fn upload_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<UploadStatusQuery>,
) -> Response {
    if !authed(&headers, &state.sessions) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    for entry in state.uploads.iter() {
        let s = entry.value();
        if s.name == q.name && s.size == q.size {
            return axum::Json(serde_json::json!({
                "token": entry.key(),
                "received": s.received,
            }))
            .into_response();
        }
    }
    StatusCode::NOT_FOUND.into_response()
}

/// Live transcodings per library video, for the subtle library-page hint:
/// [{name, kind ("audio"|"sub"), progress}] — only in-flight jobs.
async fn upload_transcodes(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authed(&headers, &state.sessions) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if state.extracting.is_empty() {
        return axum::Json(serde_json::json!([])).into_response();
    }
    let mut out = Vec::new();
    for (name, _) in library_entries(&state.media_dir).await {
        if safe_media_name(&name).is_none() {
            continue;
        }
        let stem = media_stem(&name).to_string();
        for entry in state.extracting.iter() {
            let cache = entry.key();
            if !cache.starts_with(&format!("{}.", stem)) || !is_cache_file(cache) {
                continue;
            }
            let kind = if cache.ends_with(".m4a") { "audio" } else { "sub" };
            out.push(serde_json::json!({
                "name": name,
                "kind": kind,
                "progress": extract_pct(&state, cache),
            }));
        }
    }
    axum::Json(serde_json::json!(out)).into_response()
}
/// Drop sessions idle >30min with their tmps; also purges stray tmps.
async fn sweep_uploads(state: &AppState) {
    let stale: Vec<(String, PathBuf)> = state
        .uploads
        .iter()
        .filter(|e| e.value().updated.elapsed() > std::time::Duration::from_secs(1800))
        .map(|e| (e.key().clone(), e.value().tmp.clone()))
        .collect();
    for (token, tmp) in stale {
        state.uploads.remove(&token);
        let _ = tokio::fs::remove_file(&tmp).await;
    }
}

/// Cloud-upload glyph (light on transparent, matches the dark UI).
const FAVICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" height="24" viewBox="0 -960 960 960" width="24" fill="#e3e3e3"><path d="M467-120q-73-1-136-14t-110-34.5q-47-21.5-74-50T120-280q0 33 27 61.5t74 50Q268-147 331-134t136 14Zm-15-200q-38-2-73.5-6.5t-67.5-12q-32-7.5-60-17.5t-51-23q23 13 51 23t60 17.5q32 7.5 67.5 12T452-320Zm28-279q89 0 179-26.5T760-679q-11-29-100.5-55T480-760q-91 0-178.5 25.5T200-679q14 27 101.5 53.5T480-599Zm220 479h40v-164l72 72 28-28-120-120-120 120 28 28 72-72v164ZM578.5-98.5Q520-157 520-240t58.5-141.5Q637-440 720-440t141.5 58.5Q920-323 920-240T861.5-98.5Q803-40 720-40T578.5-98.5ZM443-201q3 22 9 42t15 39q-73-1-136-14t-110-34.5q-47-21.5-74-50T120-280v-400q0-66 105.5-113T480-840q149 0 254.5 47T840-680v187q-19-9-39-15t-41-9v-62q-52 29-124 44t-156 15q-85 0-157-15t-123-44v101q51 47 130.5 62.5T480-400h11q-13 18-22.5 38T452-320q-76-4-141-18.5T200-379v99q7 13 30 26.5t56 24q33 10.5 73.5 18T443-201Z"/></svg>"##;

async fn favicon() -> Response {
    (
        [(header::CONTENT_TYPE, "image/svg+xml")],
        FAVICON_SVG,
    )
        .into_response()
}

/// Storage name for an uploaded caption: <video-stem>.<label>.vtt so
/// several languages coexist; bare names collapse to <video-stem>.vtt.
/// Typical subtitle names like `movie.pl.vtt` yield label `pl`.
fn caption_file_name(video: &str, uploaded: Option<&str>) -> String {
    let stem = media_stem(video);
    let raw = uploaded
        .and_then(|n| n.strip_suffix(".vtt").or_else(|| n.strip_suffix(".VTT")))
        .map(|s| s.rsplit('/').next().unwrap_or(s).rsplit('\\').next().unwrap_or(s))
        .unwrap_or("");
    let mut label = if raw.eq_ignore_ascii_case(stem) || raw.is_empty() {
        String::new()
    } else if let Some(rest) = raw
        .strip_prefix(stem)
        .or_else(|| raw.strip_prefix(&stem.to_ascii_lowercase()))
        .or_else(|| raw.strip_prefix(&stem.to_ascii_uppercase()))
    {
        // `Movie.en`, `Movie-EN`, `Movie EN` → `en` / `EN`.
        rest.trim_start_matches(['.', '-', '_', ' ']).to_string()
    } else {
        // `teenwolf.pl` → `teenwolf-pl`; dots collapse to dashes.
        raw.replace('.', "-")
    };
    // Dots never survive (they would fake extra extensions); keep the
    // remainder only if it is otherwise filename-safe.
    label = label.replace('.', "-");
    // `teenwolf-pl` → `pl`: a trailing 2–3 letter segment reads as the
    // language tag and becomes the label on its own.
    if let Some(tail) = label.rsplit('-').next() {
        if tail.len() >= 2 && tail.len() <= 3 && tail.chars().all(|c| c.is_ascii_alphabetic()) {
            label = tail.to_string();
        }
    }
    if label.is_empty()
        || !label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return format!("{}.vtt", stem);
    }
    format!("{}.{}.vtt", stem, label)
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
    if let Some(name) = safe_file_name(&form.name) {
        let _ = tokio::fs::remove_file(state.media_dir.join(&name)).await;
        // Deleting a video takes all its affiliated subtitle sidecars with it.
        if is_video(&name) {
            for (_, vtt) in affiliated_captions(&state.media_dir, &name).await {
                let _ = tokio::fs::remove_file(state.media_dir.join(vtt)).await;
            }
            // ...plus every extraction-cache sidecar for this video.
            if let Ok(mut rd) = tokio::fs::read_dir(&state.media_dir).await {
                while let Ok(Some(entry)) = rd.next_entry().await {
                    if let Some(s) = entry.file_name().to_str() {
                        let stem = media_stem(&name);
                        if (s.starts_with(&format!("{}.sub", stem))
                            || s.starts_with(&format!("{}.au", stem)))
                            && is_cache_file(s)
                        {
                            let _ = tokio::fs::remove_file(entry.path()).await;
                        }
                        // Orphaned temp files from killed extractions.
                        if s.starts_with(stem) && s.ends_with(".part") {
                            let _ = tokio::fs::remove_file(entry.path()).await;
                        }
                    }
                }
            }
        }
    }
    Redirect::to("/upload").into_response()
}

fn player_page(title: &str, media_url: &str, tracks: &[(String, String)]) -> Html<String> {
    let esc_title = html_escape(title);
    let mut track_tags = String::new();
    for (i, (label, url)) in tracks.iter().enumerate() {
        let def = if i == 0 { " default" } else { "" };
        track_tags.push_str(&format!(
            r#"<track kind="subtitles" srclang="en" label="{}" src="{}"{}>"#,
            html_escape(label),
            url,
            def
        ));
    }
    let track_labels = tracks
        .iter()
        .map(|(l, _)| format!("\"{}\"", l.replace('"', "")))
        .collect::<Vec<_>>()
        .join(",");
    Html(format!(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title>
<link rel="icon" type="image/svg+xml" href="/media/favicon.svg">
<style>
*{{box-sizing:border-box;margin:0;padding:0}}
html{{background:#000;user-select:none;-webkit-user-select:none}}
body{{background:#000;color:#f4f4f5;font-family:monospace;min-height:100vh;min-height:100dvh;display:flex;flex-direction:column}}
.top{{display:flex;align-items:center;gap:1rem;padding:.9rem 1.2rem}}
.top a{{color:#a1a1aa;text-decoration:none;font-size:.85rem}}
.top h1{{font-size:.95rem;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}}
.stage{{position:relative;flex:1;display:flex;background:#000;min-height:0;align-items:center;justify-content:center}}
.stage:not(.show-controls):not(.paused),.stage:not(.show-controls):not(.paused) *{{cursor:none}}
video{{width:100%;height:100%;max-height:calc(100vh - 160px);max-height:calc(100dvh - 160px);background:#000;object-fit:contain}}
.stage:fullscreen video{{max-height:100%}}
.controls{{position:absolute;left:0;right:0;bottom:0;z-index:5;padding:.6rem .9rem calc(.9rem + env(safe-area-inset-bottom));background:linear-gradient(transparent,rgba(0,0,0,.85));opacity:0;transition:opacity .25s;pointer-events:none}}
.controls button,.controls input{{pointer-events:auto}}
.stage.show-controls .controls,.stage.paused .controls{{opacity:1}}
.seek{{width:100%;accent-color:#f4f4f5;cursor:pointer;height:28px}}
.row{{display:flex;align-items:center;gap:.6rem;margin-top:.4rem}}
.ctl{{background:rgba(20,20,22,.85);border:1px solid #71717a;color:#f4f4f5;border-radius:10px;min-width:48px;min-height:48px;font:inherit;font-size:1rem;cursor:pointer;display:inline-flex;align-items:center;justify-content:center;padding:0 .6rem}}
.ctl:active{{background:#52525b}}
.ctl.on{{background:#f4f4f5;color:#000;border-color:#f4f4f5}}
.time{{font-size:.78rem;color:#e4e4e7;white-space:nowrap}}
.spacer{{flex:1}}
.center-play{{position:absolute;top:50%;left:50%;transform:translate(-50%,-50%);z-index:6;width:88px;height:88px;border-radius:50%;background:rgba(0,0,0,.65);border:2px solid #f4f4f5;color:#f4f4f5;font-size:2rem;display:flex;align-items:center;justify-content:center;cursor:pointer}}
.stage.playing .center-play{{display:none}}
.spinner{{position:absolute;top:50%;left:50%;transform:translate(-50%,-50%);z-index:7;width:56px;height:56px;border-radius:50%;border:4px solid #52525b;border-top-color:#f4f4f5;animation:spin 0.9s linear infinite;display:none}}
.stage.buffering .spinner{{display:block}}
.stage.buffering .center-play{{display:none}}
@keyframes spin{{to{{transform:translate(-50%,-50%) rotate(360deg)}}}}
.cc-wrap{{position:relative}}
.cc-menu{{position:absolute;right:0;bottom:56px;z-index:8;min-width:200px;background:rgba(10,10,12,.97);border:1px solid #52525b;border-radius:10px;padding:.4rem;display:none;flex-direction:column;gap:.15rem}}
.cc-menu.open{{display:flex}}
.cc-menu button{{background:transparent;border:none;color:#e4e4e7;border-radius:6px;min-height:44px;font:inherit;font-size:.85rem;cursor:pointer;text-align:left;padding:.5rem .8rem;display:block;width:100%}}
.cc-menu button.sel{{background:#27272a;color:#fff}}
.cc-menu button.up{{color:#a1a1aa;border-top:1px solid #27272a;margin-top:.2rem}}
.resume-veil{{position:absolute;inset:0;z-index:9;background:rgba(0,0,0,.72);display:none;align-items:center;justify-content:center}}
.toast{{position:absolute;left:50%;bottom:110px;transform:translateX(-50%);z-index:10;background:rgba(20,20,22,.95);border:1px solid #52525b;border-radius:10px;padding:.6rem 1rem;font-size:.8rem;color:#f4f4f5;display:none;white-space:nowrap;max-width:90vw;overflow:hidden;text-overflow:ellipsis}}
.toast.show{{display:block}}
.resume-veil.open{{display:flex}}
.resume-card{{border:1px solid #52525b;border-radius:12px;padding:1.4rem;display:flex;flex-direction:column;gap:.9rem;max-width:min(360px,90vw);background:#0a0a0c}}
.resume-card p{{font-size:.9rem}}
.resume-card .row{{margin-top:0}}
.cc-menu .szrow{{display:flex;gap:.3rem;align-items:center}}
.cc-menu .szrow button{{flex:1;text-align:center}}
.cc-menu .szval{{color:#f4f4f5;font-size:.8rem;min-width:44px;text-align:center}}
::cue{{background:rgba(0,0,0,.75);color:#fff;font-family:monospace}}
</style><style id="cue-style"></style></head><body>
<div class="top"><a href="/upload">&#8592; Library</a><h1>{title}</h1></div>
<div class="stage paused" id="stage">
<video id="v" playsinline preload="metadata" crossorigin="anonymous"><source src="{media}" type="{mime}">{tracks}</video>
<audio id="a" preload="auto"></audio>
<div class="spinner" id="spin"></div>
<div class="center-play" id="bigplay"><svg viewBox="0 0 24 24" width="38" height="38" fill="currentColor" aria-hidden="true"><path d="M8 5v14l11-7z"/></svg></div>
<div class="resume-veil" id="veil"><div class="resume-card">
<p id="resume-text">Resume?</p>
<div class="row"><button class="ctl" id="resume-no">Start over</button><button class="ctl" id="resume-yes">Resume</button></div>
</div></div>
<div class="toast" id="toast"></div>
<div class="controls" id="controls">
<input class="seek" id="seek" type="range" min="0" max="1000" value="0" step="1" aria-label="Seek">
<div class="row">
<button class="ctl icon" id="play" aria-label="Play/Pause"><svg id="play-ic" viewBox="0 0 24 24" width="22" height="22" fill="currentColor" aria-hidden="true"><path d="M8 5v14l11-7z"/></svg></button>
<span class="time"><span id="cur">0:00</span> / <span id="dur">0:00</span></span>
<span class="spacer"></span>
<button class="ctl" id="spd" aria-label="Speed">1x</button>
<span class="cc-wrap"><button class="ctl" id="cc" aria-label="Subtitles">CC</button><span class="cc-menu" id="ccmenu"></span></span>
<span class="cc-wrap"><button class="ctl" id="au" aria-label="Audio track">AU</button><span class="cc-menu" id="aumenu"></span></span>
<button class="ctl" id="fs" aria-label="Fullscreen">&#10530;</button>
</div></div></div>
<input type="file" id="ccfile" accept=".vtt,text/vtt" style="display:none">
<script>
(function(){{
var v=document.getElementById('v'),stage=document.getElementById('stage'),
seek=document.getElementById('seek'),play=document.getElementById('play'),
cur=document.getElementById('cur'),dur=document.getElementById('dur'),
cc=document.getElementById('cc'),ccmenu=document.getElementById('ccmenu'),
au=document.getElementById('au'),aumenu=document.getElementById('aumenu'),
aud=document.getElementById('a'),
ccfile=document.getElementById('ccfile'),fs=document.getElementById('fs'),
big=document.getElementById('bigplay'),veil=document.getElementById('veil'),
rtext=document.getElementById('resume-text'),ryes=document.getElementById('resume-yes'),
rno=document.getElementById('resume-no'),spd=document.getElementById('spd'),
toast=document.getElementById('toast'),
hideT=null,seeking=false;
var VKEY='beam-pos-'+decodeURIComponent('{media}').split('/').pop();
var CKEY='beam-cc-'+decodeURIComponent('{media}').split('/').pop();
var LABELS=[{labels}];
function fmt(s){{s=Math.max(0,Math.floor(s||0));var m=Math.floor(s/60),h=Math.floor(m/60);m=m%60;s=s%60;return (h>0?h+':'+String(m).padStart(2,'0'):m)+':'+String(s).padStart(2,'0');}}
function syncPlay(){{var playing=!v.paused&&!v.ended;stage.classList.toggle('playing',playing);stage.classList.toggle('paused',!playing);document.getElementById('play-ic').innerHTML=playing?'<path d="M6 5h4v14H6zM14 5h4v14h-4z"/>':'<path d="M8 5v14l11-7z"/>';poke();}}
function setBuffering(on){{stage.classList.toggle('buffering',on);}}
v.addEventListener('waiting',function(){{setBuffering(true);}});
v.addEventListener('stalled',function(){{setBuffering(true);}});
v.addEventListener('playing',function(){{setBuffering(false);}});
v.addEventListener('canplay',function(){{setBuffering(false);}});
v.addEventListener('seeking',function(){{setBuffering(true);}});
v.addEventListener('seeked',function(){{setBuffering(false);}});
var toastT=null;
function showToast(txt,ms){{toast.textContent=txt;toast.classList.add('show');clearTimeout(toastT);toastT=setTimeout(function(){{toast.classList.remove('show');}},ms||4000);}}
function pmb(n){{return (n/1048576).toFixed(1)+' MB';}}
function xpct(t){{if(t.progress==null)return ' …';return ' ('+t.progress+'%'+(t.partial!=null?' · '+pmb(t.partial):'')+')';}}
function poke(){{stage.classList.add('show-controls');clearTimeout(hideT);if(!v.paused)hideT=setTimeout(function(){{stage.classList.remove('show-controls');ccmenu.classList.remove('open');aumenu.classList.remove('open');}},2800);}}
function auActive(){{return aud.hasAttribute('src')&&aud.getAttribute('src')!=='';}}
function syncAuToVideo(){{if(!auActive())return;try{{if(aud.readyState>0&&Math.abs(v.currentTime-aud.currentTime)>0.05)aud.currentTime=v.currentTime;}}catch(_e){{}}aud.playbackRate=v.playbackRate;}}
function playAll(){{v.play();if(auActive()){{syncAuToVideo();var p=aud.play();if(p&&p.catch)p.catch(function(){{}});}}}}
function pauseAll(){{v.pause();if(auActive())aud.pause();}}
function toggle(){{if(v.paused)playAll();else pauseAll();}}
play.addEventListener('click',function(e){{e.stopPropagation();toggle();}});
big.addEventListener('click',function(e){{e.stopPropagation();toggle();}});
v.addEventListener('click',toggle);
v.onplay=syncPlay;v.onpause=function(){{syncPlay();if(auActive())aud.pause();}};v.onplaying=function(){{if(auActive()){{syncAuToVideo();var p=aud.play();if(p&&p.catch)p.catch(function(){{}});}}}};v.onended=function(){{syncPlay();if(auActive())aud.pause();try{{localStorage.removeItem(VKEY);}}catch(_e){{}}}};
v.addEventListener('seeked',function(){{setBuffering(false);syncAuToVideo();}});
v.onloadedmetadata=function(){{dur.textContent=fmt(v.duration);restoreCC();maybeResume();}};
v.ontimeupdate=function(){{cur.textContent=fmt(v.currentTime);if(v.duration&&!seeking)seek.value=Math.round(v.currentTime/v.duration*1000);if(auActive()&&aud.readyState>0&&!v.paused&&!v.seeking){{try{{if(Math.abs(v.currentTime-aud.currentTime)>0.35)aud.currentTime=v.currentTime;}}catch(_e){{}}}}}};
seek.addEventListener('pointerdown',function(){{seeking=true;}});
seek.addEventListener('pointerup',function(){{seeking=false;}});
seek.addEventListener('input',function(){{if(v.duration){{v.currentTime=seek.value/1000*v.duration;syncAuToVideo();}}}});
setInterval(function(){{if(v.duration&&!v.paused)try{{localStorage.setItem(VKEY,String(v.currentTime));}}catch(_e){{}}}},5000);
window.addEventListener('pagehide',function(){{try{{if(v.duration&&v.currentTime>1)localStorage.setItem(VKEY,String(v.currentTime));}}catch(_e){{}}}});
function maybeResume(){{var t=0;try{{t=parseFloat(localStorage.getItem(VKEY))||'0';}}catch(_e){{}}if(t>10&&v.duration&&t<v.duration-10){{rtext.textContent='Resume from '+fmt(t)+'?';veil.classList.add('open');rno.textContent='Start over';delete rno.dataset.armed;ryes.onclick=function(){{v.currentTime=t;veil.classList.remove('open');v.play();}};rno.onclick=function(){{if(!rno.dataset.armed){{rno.dataset.armed='1';rno.textContent='Are you sure?';setTimeout(function(){{delete rno.dataset.armed;rno.textContent='Start over';}},3000);return;}}try{{localStorage.removeItem(VKEY);}}catch(_e){{}}veil.classList.remove('open');v.currentTime=0;v.play();}};}}}}
function trackList(){{return v.textTracks;}}
function fmtUp(l){{return /^[a-z]{{2,3}}$/i.test(l)?'['+l.toUpperCase()+']':l;}}
// Unified subtitle state: 'off' | 'u'<uploaded idx> | 'e'<embedded stream idx>
var XKEY='beam-ccx-'+decodeURIComponent('{media}').split('/').pop();
var EMB=[],wantSub=null;
var xt=document.createElement('track');xt.kind='subtitles';xt.id='xtext';v.appendChild(xt);
function savedSel(){{try{{var s=localStorage.getItem(XKEY);if(s==='off'||s[0]==='u'||s[0]==='e')return s;}}catch(_e){{}}return 'u0';}}
function applySel(sel){{var ts=trackList();for(var i=0;i<ts.length;i++)ts[i].mode='hidden';if(sel[0]==='u'){{var ui=parseInt(sel.slice(1),10);if(!isNaN(ui)&&ts[ui])ts[ui].mode='showing';}}else if(sel[0]==='e'){{if(xt.track)xt.track.mode='showing';}}try{{localStorage.setItem(XKEY,sel);}}catch(_e){{}}cc.classList.toggle('on',sel!=='off');buildMenu(sel);}}
function curEmbIdx(){{return xt.dataset.idx!==undefined&&xt.dataset.idx!==''?parseInt(xt.dataset.idx,10):-1;}}
function currentSel(){{var ts=trackList();for(var i=0;i<ts.length;i++)if(ts[i].mode==='showing')return 'u'+i;if(xt.track&&xt.track.mode==='showing')return 'e'+curEmbIdx();return 'off';}}
function showEmbedded(eidx){{var em=null;for(var i=0;i<EMB.length;i++)if(EMB[i].index===eidx)em=EMB[i];if(!em||!em.cached)return false;xt.src=em.url;xt.dataset.idx=String(eidx);var done=false;function ready(){{if(done)return;done=true;applySel('e'+eidx);}}xt.addEventListener('load',function onl(){{xt.removeEventListener('load',onl);ready();}});setTimeout(ready,1200);return true;}}
function vname(){{return decodeURIComponent('{media}').split('/').pop();}}
function prepareTrack(kind,idx){{var fd=new FormData();fd.append('v',vname());fd.append('kind',kind);fd.append('index',String(idx));fetch('/tracks/prepare',{{method:'POST',body:fd,credentials:'same-origin'}}).then(function(){{}}).catch(function(){{}});}}
function refreshTracks(){{fetch('/tracks?v='+encodeURIComponent(vname()),{{credentials:'same-origin'}}).then(function(r){{return r.ok?r.json():null;}}).then(function(j){{if(!j)return;if(j.subs){{EMB=j.subs;if(ccmenu.classList.contains('open'))buildMenu(currentSel());if(wantSub!==null){{var em=null;for(var i=0;i<EMB.length;i++)if(EMB[i].index===wantSub)em=EMB[i];if(em&&em.cached){{wantSub=null;showEmbedded(em.index);}}else if(em&&!em.failed){{prepareTrack('subs',em.index);if(em.progress!=null)showToast('Transcoding:'+xpct(em));}}else if(em&&em.failed){{showToast('Subtitles failed — open CC to retry');}}}}}}if(j.audio){{AUD=j.audio;if(aumenu.classList.contains('open'))buildAuMenu(curAuIdx());if(wantAu!==null){{var at=null;for(var k=0;k<AUD.length;k++)if(AUD[k].index===wantAu)at=AUD[k];if(at&&at.cached){{wantAu=null;applyAu(at.index);showToast('Audio ready');}}else if(at&&!at.failed){{prepareTrack('audio',at.index);if(at.progress!=null)showToast('Transcoding:'+xpct(at));}}else if(at&&at.failed){{showToast('Transcoding failed — open AU to retry');}}}}if(!auRestored){{auRestored=true;if(!restoreAu())autoDefaultAudio();}}}}}}).catch(function(){{}});}}
setInterval(function(){{if(wantSub!==null||wantAu!==null)refreshTracks();}},2000);
// Audio track chooser: the container default plays natively; anything else
// (or an undecodable default like EAC3) plays via an extracted AAC sidecar
// through the hidden <audio> element, frame-synced to the muted video.
var AUD=[],wantAu=null,auRestored=false;
var AUKEY='beam-au-'+decodeURIComponent('{media}').split('/').pop();
function auTrack(idx){{for(var i=0;i<AUD.length;i++)if(AUD[i].index===idx)return AUD[i];return null;}}
function curAuIdx(){{if(auActive()){{var m=/track=(\d+)/.exec(aud.getAttribute('src')||'');return m?parseInt(m[1],10):-2;}}for(var i=0;i<AUD.length;i++)if(AUD[i].default)return AUD[i].index;return -2;}}
function applyAu(idx){{var t=auTrack(idx);
if(idx===-1||(t&&t.default&&t.native)){{v.muted=false;aud.pause();aud.removeAttribute('src');aud.load();au.classList.remove('on');try{{localStorage.setItem(AUKEY,'native');}}catch(_e){{}}buildAuMenu(curAuIdx());return;}}
if(!t||!t.cached){{if(t){{wantAu=t.index;prepareTrack('audio',t.index);showToast('Extracting audio, sound starts automatically');}}buildAuMenu(curAuIdx());return;}}
wantAu=null;v.muted=true;aud.src=t.url;aud.playbackRate=v.playbackRate;syncAuToVideo();au.classList.add('on');try{{localStorage.setItem(AUKEY,String(idx));}}catch(_e){{}}if(!v.paused){{var p=aud.play();if(p&&p.catch)p.catch(function(){{}});}}buildAuMenu(curAuIdx());}}
function restoreAu(){{var s=null;try{{s=localStorage.getItem(AUKEY);}}catch(_e){{}}if(s===null||s==='')return false;if(s==='native'){{applyAu(-1);return true;}}var idx=parseInt(s,10);if(isNaN(idx))return false;var t=auTrack(idx);if(!t)return false;if(t.default&&t.native)applyAu(-1);else if(t.cached)applyAu(idx);else{{wantAu=idx;prepareTrack('audio',idx);showToast('Extracting audio, sound starts automatically');}}return true;}}
function autoDefaultAudio(){{var d=null;for(var i=0;i<AUD.length;i++)if(AUD[i].default)d=AUD[i];if(!d||d.native)return;if(d.cached){{applyAu(d.index);}}else{{wantAu=d.index;prepareTrack('audio',d.index);showToast('No playable sound in file, extracting audio…');}}}}
function buildAuMenu(sel){{aumenu.innerHTML='';function add(txt,val){{var b=document.createElement('button');b.textContent=txt;if(val===sel)b.classList.add('sel');b.onclick=function(ev){{ev.stopPropagation();applyAu(val);aumenu.classList.remove('open');}};aumenu.appendChild(b);}}for(var i=0;i<AUD.length;i++){{var t=AUD[i];var tag=t.label+(t.default?' (default)':'')+((t.failed&&!t.cached)?' (failed — retry)':((!t.native&&!t.cached)||(t.native&&!t.default&&!t.cached)?xpct(t):''));add(tag,t.index);}}if(!AUD.length)add('No audio tracks',-2);}}
au.addEventListener('click',function(e){{e.stopPropagation();buildAuMenu(curAuIdx());aumenu.classList.toggle('open');poke();}});
function applyCC(idx){{applySel(idx<0?'off':'u'+idx);}}
function savedCC(){{var s=savedSel();return s[0]==='u'?parseInt(s.slice(1),10):-1;}}
function restoreCC(){{var ts=trackList();var s=savedSel();if(s[0]==='u'){{var i=parseInt(s.slice(1),10);if(isNaN(i)||i<-1||i>=ts.length)i=ts.length?0:-1;applySel(i<0?'off':'u'+i);}}else if(s[0]==='e'){{var ei=parseInt(s.slice(1),10);var em=null;for(var j=0;j<EMB.length;j++)if(EMB[j].index===ei)em=EMB[j];if(em&&em.cached)showEmbedded(ei);else applySel('off');}}else applySel('off');}}
function buildMenu(sel){{ccmenu.innerHTML='';function add(txt,val){{var b=document.createElement('button');b.textContent=txt;if(val===sel)b.classList.add('sel');b.onclick=function(ev){{ev.stopPropagation();if(val[0]==='e'){{var ei=parseInt(val.slice(1),10);if(!showEmbedded(ei)){{wantSub=ei;prepareTrack('subs',ei);buildMenu(sel);return;}}}}else applySel(val);ccmenu.classList.remove('open');}};ccmenu.appendChild(b);}}add('Off','off');for(var i=0;i<LABELS.length;i++)add(fmtUp(LABELS[i]),'u'+i);for(var k=0;k<EMB.length;k++)add(EMB[k].label+(EMB[k].cached?'':(EMB[k].failed?' (failed — retry)':xpct(EMB[k]))),'e'+EMB[k].index);var sz=document.createElement('div');sz.className='szrow';var dm=document.createElement('button');dm.textContent='A-';dm.onclick=function(ev){{ev.stopPropagation();bumpCue(-2);}};var sv=document.createElement('span');sv.className='szval';sv.id='szval';sv.textContent=cueSize+'px';var up2=document.createElement('button');up2.textContent='A+';up2.onclick=function(ev){{ev.stopPropagation();bumpCue(2);}};sz.appendChild(dm);sz.appendChild(sv);sz.appendChild(up2);ccmenu.appendChild(sz);var up=document.createElement('button');up.textContent='+ Upload .vtt';up.classList.add('up');up.onclick=function(ev){{ev.stopPropagation();ccmenu.classList.remove('open');ccfile.click();}};ccmenu.appendChild(up);}}
var cueStyle=document.getElementById('cue-style'),cueSize=28;
try{{var s=parseInt(localStorage.getItem('beam-cue-size'),10);if(s>=12&&s<=48)cueSize=s;}}catch(_e){{}}
function applyCue(){{cueStyle.textContent='::cue{{font-size:'+cueSize+'px;color:#fff;background:rgba(0,0,0,.6);text-shadow:-1px 0 0 #000,1px 0 0 #000,0 -1px 0 #000,0 1px 0 #000, -1px -1px 0 #000,1px 1px 0 #000,1px -1px 0 #000,-1px 1px 0 #000;}}';try{{localStorage.setItem('beam-cue-size',String(cueSize));}}catch(_e){{}}var sv=document.getElementById('szval');if(sv)sv.textContent=cueSize+'px';}}
function bumpCue(d){{cueSize=Math.min(48,Math.max(12,cueSize+d));applyCue();}}
applyCue();
cc.addEventListener('click',function(e){{e.stopPropagation();buildMenu(currentSel());ccmenu.classList.toggle('open');poke();}});
ccfile.addEventListener('change',function(){{var f=ccfile.files[0];if(!f)return;if(f.size>50*1024*1024||!f.size){{ccfile.value='';return;}}var vn=decodeURIComponent('{media}').split('/').pop();upSidecar(f,vn).then(function(ok){{if(ok)location.reload();}}).catch(function(){{}});ccfile.value='';}});
function upSidecar(f,target){{var CH=8*1024*1024;function qp(p,init){{return fetch(p,init);}}
return qp('/upload/start',{{method:'POST',body:new URLSearchParams({{name:f.name,size:String(f.size),kind:'caption',for_video:target}}),credentials:'same-origin'}}).then(function(r){{if(!r.ok)throw 0;return r.json();}}).then(function(sj){{var off=sj.received||0;
function next(){{if(off>=f.size){{return qp('/upload/finish',{{method:'POST',headers:{{'Content-Type':'application/x-www-form-urlencoded'}},body:'token='+encodeURIComponent(sj.token),credentials:'same-origin'}}).then(function(r){{return r.ok;}});}}
return qp('/upload/chunk?token='+encodeURIComponent(sj.token)+'&offset='+off,{{method:'POST',body:f.slice(off,Math.min(off+CH,f.size)),credentials:'same-origin'}}).then(function(r){{if(r.status===409)return r.json().then(function(j){{off=j.received;return next();}});if(!r.ok)throw 0;return r.json();}}).then(function(j){{if(j&&j.received!==undefined)off=j.received;return next();}});}}
return next();}});}}
fs.addEventListener('click',function(e){{e.stopPropagation();if(document.fullscreenElement)document.exitFullscreen();else if(stage.requestFullscreen)stage.requestFullscreen();else if(v.webkitEnterFullscreen)v.webkitEnterFullscreen();}});
document.addEventListener('click',function(e){{if(!e.target.closest||!e.target.closest('.cc-wrap')){{ccmenu.classList.remove('open');aumenu.classList.remove('open');}}}});
document.addEventListener('click',function(e){{var b=e.target.closest?e.target.closest('button'):null;if(b)b.blur();}});
var SPEEDS=[1,1.25,1.5,2,0.5],spdIx=0,holdT0=0,holdingSpace=false,restoreRate=1;
function rateLabel(r){{return (Math.round(r*100)/100)+'x';}}
function applyBaseRate(){{v.playbackRate=SPEEDS[spdIx];if(auActive())aud.playbackRate=v.playbackRate;spd.textContent=rateLabel(SPEEDS[spdIx]);}}
spd.addEventListener('click',function(e){{e.stopPropagation();if(holdingSpace)return;spdIx=(spdIx+1)%SPEEDS.length;applyBaseRate();poke();}});
document.onkeydown=function(e){{
if(e.target&&e.target.tagName==='INPUT'&&e.target!==seek)return;
if(e.code==='Space'){{e.preventDefault();if(e.repeat)return;holdT0=Date.now();holdingSpace=true;restoreRate=SPEEDS[spdIx];v.playbackRate=2;if(auActive())aud.playbackRate=2;spd.textContent='2x';poke();}}
else if(e.key==='ArrowRight')v.currentTime+=10;
else if(e.key==='ArrowLeft')v.currentTime-=10;
else if(e.key==='f'||e.key==='F')fs.click();
}};
document.onkeyup=function(e){{
if(e.code==='Space'&&holdingSpace){{holdingSpace=false;var held=Date.now()-holdT0;v.playbackRate=restoreRate;if(auActive())aud.playbackRate=restoreRate;spd.textContent=rateLabel(restoreRate);if(held<300)toggle();}}
}};
stage.addEventListener('mousemove',poke);stage.addEventListener('touchstart',poke,{{passive:true}});
document.addEventListener('visibilitychange',function(){{if(document.hidden&&!v.paused)pauseAll();}});
buildMenu('off');refreshTracks();poke();
}})();
</script></body></html>"#,
        title = esc_title,
        media = media_url,
        mime = media_content_type(title),
        tracks = track_tags,
        labels = track_labels
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
    let mut tracks = Vec::new();
    for (label, file) in affiliated_captions(&state.media_dir, &name).await {
        // Affiliated files were just scanned from our own dir; re-validate.
        if file.contains('/') || file.contains('\\') || file.contains("..") {
            continue;
        }
        tracks.push((label, format!("/captions/{}", url_encode(&file))));
    }
    player_page(
        &name,
        &format!("/media/{}", url_encode(&name)),
        &tracks,
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

#[derive(Deserialize)]
struct MediaQuery {
    dl: Option<String>,
}

/// Stream a file with HTTP Range support so browsers play without
/// downloading: 206 Partial Content for `bytes=start-end`, 200 otherwise.
/// Reads in 64KB chunks — never buffers the file in RAM. Browser-native
/// types (player video, images, PDFs, audio) render inline; everything
/// else (and ?dl=1) downloads as an attachment.
async fn media_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(q): Query<MediaQuery>,
) -> Response {
    if !authed(&headers, &state.sessions) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(safe) = safe_file_name(&name) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let path = state.media_dir.join(&safe);
    let Ok(meta) = tokio::fs::metadata(&path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if inline_native(&safe) && q.dl.is_none() {
        return stream_range_response(
            path,
            meta.len(),
            media_content_type(&safe),
            None,
            headers.get(header::RANGE),
        )
        .await;
    }
    stream_range_response(
        path,
        meta.len(),
        "application/octet-stream",
        Some(format!("attachment; filename=\"{}\"", safe.replace('"', ""))),
        headers.get(header::RANGE),
    )
    .await
}

/// Shared 206/200 range-streaming core: 64KB chunks, never buffers the file.
/// `download`: Some(content-disposition) forces a download instead of inline.
async fn stream_range_response(
    path: PathBuf,
    total: u64,
    content_type: &'static str,
    download: Option<String>,
    range: Option<&HeaderValue>,
) -> Response {
    if total == 0 {
        return StatusCode::NOT_FOUND.into_response();
    }
    let has_range = range.is_some();
    let (start, end) = match range
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
    let mut builder = Response::builder()
        .status(if has_range {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(header::CONTENT_TYPE, content_type)
        .header("X-Content-Type-Options", "nosniff")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_RANGE, content_range)
        .header(header::CONTENT_LENGTH, length);
    if let Some(disposition) = download {
        builder = builder.header(header::CONTENT_DISPOSITION, disposition);
    }
    builder
        .body(Body::from_stream(stream))
        .unwrap()
        .into_response()
}

// ============ TRACK PROBING + ON-DEMAND SUBTITLE EXTRACTION ============
// Browsers read subtitles only from <track> files, so embedded subtitle
// tracks are extracted on demand with ffmpeg into sidecar cache files:
//   subs:  <stem>.sub<idx>.vtt            (WebVTT, converted)
// Cache files never appear in the library and die with their video.
// NOTE: HTML5 video plays the container default audio track only, and
// browsers cannot decode EAC3/AC3/DTS at all — so alternate tracks (and any
// undecodable default) are extracted once to AAC sidecars below and played
// through a synced <audio> element while the video stays muted. The video
// bytes are always the untouched original.

#[derive(Debug, Clone)]
struct SubTrackInfo {
    index: usize, // global ffprobe stream index
    lang: String,
    forced: bool,
    sdh: bool,
}

#[derive(Debug, Clone)]
struct AudioTrackInfo {
    index: usize, // global ffprobe stream index
    lang: String,
    title: String,
    channels: u64,
    codec: String,
    is_default: bool,
}

/// Codecs a browser <audio>/<video> element can decode natively.
fn audio_native_playable(codec: &str) -> bool {
    matches!(codec, "aac" | "mp3" | "opus" | "vorbis")
}

fn audio_label(t: &AudioTrackInfo) -> String {
    let mut s = format!("[{}]", t.lang.to_ascii_uppercase());
    if !t.title.is_empty() {
        let short: String = t.title.chars().take(40).collect();
        s.push_str(&format!(" {}", short));
    } else {
        s.push_str(&format!(" {} {}ch", t.codec.to_ascii_uppercase(), t.channels));
    }
    s
}

fn clean_lang(raw: &str) -> String {
    let l: String = raw
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    if l.is_empty() {
        "und".to_string()
    } else {
        l
    }
}

fn sub_label(lang: &str, forced: bool, sdh: bool) -> String {
    let mut s = format!("[{}]", lang.to_ascii_uppercase());
    if sdh {
        s.push_str(" SDH");
    }
    if forced {
        s.push_str(" FORCED");
    }
    s
}

async fn probe_duration_ms(path: &PathBuf) -> u64 {
    let out = tokio::process::Command::new("ffprobe")
        .args([
            "-v", "error",
            "-show_entries", "format=duration",
            "-of", "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .await;
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse::<f64>()
            .map(|s| (s * 1000.0) as u64)
            .unwrap_or(0),
        Err(_) => 0,
    }
}

/// Parse ffmpeg `-progress` out_time stamps (`HH:MM:SS.micro`, or seconds).
/// Returns milliseconds. "N/A" (nothing muxed yet) yields None.
fn parse_out_time(ts: &str) -> Option<u64> {
    if ts == "N/A" {
        return None;
    }
    let mut parts = ts.split(':');
    let h: u64 = parts.next()?.parse().ok()?;
    let m: u64 = parts.next()?.parse().ok()?;
    let rest = parts.next()?;
    let (s_str, frac) = match rest.split_once('.') {
        Some((s, f)) => (s, f),
        None => (rest, ""),
    };
    let s: u64 = s_str.parse().ok()?;
    let micro: u64 = format!("{:0<6}", frac).chars().take(6).collect::<String>().parse().ok()?;
    Some(((h * 60 + m) * 60 + s) * 1000 + micro / 1000)
}

fn extract_pct(state: &AppState, file: &str) -> Option<u64> {
    if !state.extracting.contains(file) {
        return None;
    }
    let total = state.totals.get(file).map(|v| *v).unwrap_or(0);
    if total == 0 {
        return None;
    }
    let out = state.progress.get(file).map(|v| *v).unwrap_or(0);
    Some((out * 100 / total).min(99))
}

/// Bytes written so far to the in-flight .part tmp (liveness proof while the
/// time-based percent sits at 99% on slow tails). None when not extracting.
async fn extract_partial(state: &AppState, file: &str) -> Option<u64> {
    if !state.extracting.contains(file) {
        return None;
    }
    let tmp = state.media_dir.join(format!("{}.part", file));
    tokio::fs::metadata(&tmp).await.map(|m| m.len()).ok()
}

/// Shared extraction runner: single ffmpeg thread, optional CPU duty-cycle
/// cap (SIGSTOP/SIGCONT at CPU_LIMIT_PCT% of one core), machine-parsable
/// -progress feed into state.progress, atomic publish via tmp+rename.
/// Cleans up its extracting/progress/totals bookkeeping on the way out.
async fn run_extract(
    src: PathBuf,
    extra_args: Vec<String>,
    muxer: &'static str,
    tmp: PathBuf,
    dst: PathBuf,
    cache: String,
    state: AppState,
) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncBufReadExt, BufReader};
    let total = probe_duration_ms(&src).await;
    if total > 0 {
        state.totals.insert(cache.clone(), total);
    }
    state.progress.insert(cache.clone(), 0);
    let mut cmd = tokio::process::Command::new("ffmpeg");
    cmd.args([
        "-y", "-v", "error", "-progress", "pipe:1", "-nostats", "-threads", "1", "-i",
    ])
    .arg(&src);
    for a in &extra_args {
        cmd.arg(a);
    }
    cmd.args(["-f", muxer]).arg(&tmp);
    cmd.stdout(std::process::Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            state.progress.remove(&cache);
            state.totals.remove(&cache);
            state.extracting.remove(&cache);
            return;
        }
    };
    // CPU cap: run `cap`% of each second, freeze the rest. Coarse but hard.
    let cap = cpu_limit_pct();
    let done = Arc::new(AtomicBool::new(false));
    if cap > 0 && cap < 100 {
        let pid = child.id();
        let done_flag = done.clone();
        let (run_ms, stop_ms) = (cap * 10, 1000 - cap * 10);
        tokio::spawn(async move {
            use tokio::time::{sleep, Duration};
            loop {
                sleep(Duration::from_millis(run_ms)).await;
                if done_flag.load(Ordering::Relaxed) {
                    break;
                }
                if let Some(p) = pid {
                    unsafe {
                        libc::kill(p as i32, libc::SIGSTOP);
                    }
                }
                sleep(Duration::from_millis(stop_ms)).await;
                if done_flag.load(Ordering::Relaxed) {
                    break;
                }
                if let Some(p) = pid {
                    unsafe {
                        libc::kill(p as i32, libc::SIGCONT);
                    }
                }
            }
        });
    }
    if let Some(out) = child.stdout.take() {
        let progress = state.progress.clone();
        let key = cache.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // NOTE: parse the human `out_time=HH:MM:SS.micro` stamp, NOT
                // the out_time_ms field — some builds emit microseconds in
                // the ms slot (1000x over), which pinned us at 99% forever.
                if let Some(ts) = line.strip_prefix("out_time=") {
                    if let Some(ms) = parse_out_time(ts.trim()) {
                        progress.insert(key.clone(), ms);
                    }
                } else if line.starts_with("progress=end") {
                    break;
                }
            }
        });
    }
    let ok = match child.wait().await {
        Ok(s) => s.success(),
        Err(_) => false,
    } && tokio::fs::metadata(&tmp).await.is_ok();
    done.store(true, Ordering::Relaxed);
    if ok {
        let _ = tokio::fs::rename(&tmp, &dst).await;
        state.failures.remove(&cache);
    } else {
        let _ = tokio::fs::remove_file(&tmp).await;
        *state.failures.entry(cache.clone()).or_insert(0) += 1;
    }
    state.progress.remove(&cache);
    state.totals.remove(&cache);
    state.extracting.remove(&cache);
}

async fn probe_tracks(path: &PathBuf) -> Result<(Vec<SubTrackInfo>, Vec<AudioTrackInfo>), String> {
    let out = tokio::process::Command::new("ffprobe")
        .args([
            "-v", "error",
            "-show_entries",
            "stream=index,codec_type,codec_name,channels:stream_tags=language,title:stream_disposition=forced,hearing_impaired",
            "-of", "json",
        ])
        .arg(path)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("ffprobe failed".to_string());
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let mut subs = Vec::new();
    let mut audios = Vec::new();
    if let Some(streams) = v.get("streams").and_then(|s| s.as_array()) {
        for s in streams {
            let index = s.get("index").and_then(|v| v.as_u64()).unwrap_or(999) as usize;
            let ctype = s.get("codec_type").and_then(|v| v.as_str()).unwrap_or("");
            let tags = s.get("tags");
            let lang = clean_lang(tags.and_then(|t| t.get("language")).and_then(|v| v.as_str()).unwrap_or("und"));
            let title = tags
                .and_then(|t| t.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let disp = s.get("disposition");
            let flag = |k: &str| disp.and_then(|d| d.get(k)).and_then(|v| v.as_u64()).unwrap_or(0) == 1;
            if ctype == "subtitle" {
                let sdh = flag("hearing_impaired")
                    || title.to_ascii_lowercase().contains("sdh")
                    || title.to_ascii_lowercase().contains("hearing")
                    || title.to_ascii_lowercase() == "cc";
                subs.push(SubTrackInfo {
                    index,
                    lang,
                    forced: flag("forced"),
                    sdh,
                });
            } else if ctype == "audio" {
                let codec = s
                    .get("codec_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("und")
                    .to_ascii_lowercase();
                let channels = s.get("channels").and_then(|v| v.as_u64()).unwrap_or(0);
                audios.push(AudioTrackInfo {
                    index,
                    lang,
                    title,
                    channels,
                    codec,
                    is_default: flag("default"),
                });
            }
        }
    }
    if !audios.iter().any(|a| a.is_default) {
        if let Some(first) = audios.first_mut() {
            first.is_default = true;
        }
    }
    Ok((subs, audios))
}

fn audio_cache_name(video: &str, idx: usize) -> String {
    format!("{}.au{}.m4a", media_stem(video), idx)
}

fn sub_cache_name(video: &str, idx: usize) -> String {
    format!("{}.sub{}.vtt", media_stem(video), idx)
}

fn is_cache_file(name: &str) -> bool {
    // Matches extraction-cache subtitles <stem>.sub<idx>.vtt
    // and extraction-cache audio <stem>.au<idx>.m4a.
    if let Some(base) = name.strip_suffix(".vtt") {
        if let Some(seg) = base.rsplit('.').next() {
            return seg.starts_with("sub")
                && seg[3..].chars().all(|c| c.is_ascii_digit())
                && !seg[3..].is_empty();
        }
        return false;
    }
    if let Some(base) = name.strip_suffix(".m4a") {
        if let Some(seg) = base.rsplit('.').next() {
            return seg.starts_with("au")
                && seg[2..].chars().all(|c| c.is_ascii_digit())
                && !seg[2..].is_empty();
        }
    }
    false
}

async fn tracks_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WatchQuery>,
) -> Response {
    if !authed(&headers, &state.sessions) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(video) = safe_media_name(&q.v) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = state.media_dir.join(&video);
    if tokio::fs::metadata(&path).await.is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let (subs, audios) = probe_tracks(&path).await.unwrap_or_default();
    let mut sj = Vec::new();
    for t in &subs {
        let file = sub_cache_name(&video, t.index);
        // Not cached while an extraction is still running (the file only
        // appears via atomic rename on success, this is belt-and-braces).
        let cached = !state.extracting.contains(&file)
            && tokio::fs::metadata(state.media_dir.join(&file)).await.is_ok();
        sj.push(serde_json::json!({
            "index": t.index,
            "label": sub_label(&t.lang, t.forced, t.sdh),
            "cached": cached,
            "progress": extract_pct(&state, &file),
            "partial": extract_partial(&state, &file).await,
            "failed": state.failures.get(&file).map(|v| *v).unwrap_or(0),
            "url": format!("/captions/{}", url_encode(&file)),
        }));
    }
    let mut aj = Vec::new();
    for t in &audios {
        let file = audio_cache_name(&video, t.index);
        let native = audio_native_playable(&t.codec);
        let acached = !state.extracting.contains(&file)
            && tokio::fs::metadata(state.media_dir.join(&file)).await.is_ok();
        aj.push(serde_json::json!({
            "index": t.index,
            "label": audio_label(t),
            "codec": t.codec,
            "channels": t.channels,
            "default": t.is_default,
            "native": native,
            // Undecodable defaults still need extraction; native alternates
            // never play (container default wins), so they need it too.
            "cached": acached,
            "progress": extract_pct(&state, &file),
            "partial": extract_partial(&state, &file).await,
            "failed": state.failures.get(&file).map(|v| *v).unwrap_or(0),
            "url": format!("/audio?v={}&track={}", url_encode(&video), t.index),
        }));
    }
    axum::Json(serde_json::json!({ "subs": sj, "audio": aj })).into_response()
}

#[derive(Deserialize)]
struct PrepareForm {
    v: String,
    kind: String,
    index: usize,
}

async fn tracks_prepare(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<PrepareForm>,
) -> Response {
    if !authed(&headers, &state.sessions) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(video) = safe_media_name(&form.v) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let path = state.media_dir.join(&video);
    if tokio::fs::metadata(&path).await.is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    // Re-probe to validate the index instead of trusting it blindly.
    let (subs, audios) = match probe_tracks(&path).await {
        Ok(t) => t,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let cache = if form.kind.as_str() == "subs" {
        match subs.iter().find(|t| t.index == form.index) {
            Some(t) => sub_cache_name(&video, t.index),
            None => return StatusCode::BAD_REQUEST.into_response(),
        }
    } else if form.kind.as_str() == "audio" {
        match audios.iter().find(|t| t.index == form.index) {
            Some(t) => audio_cache_name(&video, t.index),
            None => return StatusCode::BAD_REQUEST.into_response(),
        }
    } else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let is_audio = form.kind.as_str() == "audio";
    let dst = state.media_dir.join(&cache);
    // Never serve a half-written extraction: not cached while busy, and a
    // killed ffmpeg (e.g. service restart mid-extraction) leaves no moov.
    if tokio::fs::metadata(&dst).await.is_ok() || state.extracting.contains(&cache) {
        return (StatusCode::OK, "ready").into_response();
    }
    if !state.extracting.insert(cache.clone()) {
        return (StatusCode::ACCEPTED, "busy").into_response();
    }
    // Fresh attempt: old failure count must not haunt the retry.
    state.failures.remove(&cache);
    // Extract to a temp name, publish atomically on success. A crash leaves
    // only a hidden .part file (never listed, never served, wiped on delete).
    let tmp = state.media_dir.join(format!("{}.part", &cache));
    let (args, muxer) = if is_audio {
        (
            vec![
                "-map".to_string(),
                format!("0:{}", form.index),
                "-vn".to_string(),
                "-c:a".to_string(),
                "aac".to_string(),
                "-b:a".to_string(),
                "160k".to_string(),
            ],
            "mp4",
        )
    } else {
        (
            vec!["-map".to_string(), format!("0:{}", form.index)],
            "webvtt",
        )
    };
    let st = state.clone();
    tokio::spawn(run_extract(path, args, muxer, tmp, dst, cache, st));
    (StatusCode::ACCEPTED, "started").into_response()
}

#[derive(Deserialize)]
struct AudioQuery {
    v: String,
    track: usize,
}

/// Serve an extracted audio sidecar with Range support. 404 while the
/// extraction is still running (the player polls /tracks until cached).
async fn serve_audio(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AudioQuery>,
) -> Response {
    if !authed(&headers, &state.sessions) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(video) = safe_media_name(&q.v) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let file = audio_cache_name(&video, q.track);
    let path = state.media_dir.join(&file);
    // The filename is derived, but double-check it matches our cache shape
    // so crafted ?track= values can never escape the media dir.
    if !is_cache_file(&file) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Ok(meta) = tokio::fs::metadata(&path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    stream_range_response(
        path,
        meta.len(),
        "audio/mp4",
        None,
        headers.get(header::RANGE),
    )
    .await
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
        extracting: Arc::new(DashSet::new()),
        progress: Arc::new(DashMap::new()),
        totals: Arc::new(DashMap::new()),
        uploads: Arc::new(DashMap::new()),
        failures: Arc::new(DashMap::new()),
    };

    // Sweep uploads idle >30min, every 10 minutes.
    {
        let sweep_state = state.clone();
        tokio::spawn(async move {
            use tokio::time::{sleep, Duration};
            loop {
                sleep(Duration::from_secs(600)).await;
                sweep_uploads(&sweep_state).await;
            }
        });
    }

    let app = Router::new()
        .route("/upload", get(upload_page).post(upload_login))
        .route("/upload/delete", post(upload_delete))
        .route("/upload/start", post(upload_start))
        .route(
            "/upload/chunk",
            post(upload_chunk).route_layer(DefaultBodyLimit::disable()),
        )
        .route("/upload/finish", post(upload_finish))
        .route("/upload/cancel", post(upload_cancel))
        .route("/upload/status", get(upload_status))
        .route("/upload/transcodes", get(upload_transcodes))
        .route("/watch", get(watch_page))
        .route("/media/:name", get(media_file))
        .route("/captions/:name", get(captions_file))
        .route("/tracks", get(tracks_info))
        .route("/tracks/prepare", post(tracks_prepare))
        .route("/audio", get(serve_audio))
        .route("/media/favicon.svg", get(favicon))
        .with_state(state);

    // Crash leftovers from chunked uploads never survive a restart: their
    // sessions are gone, so their hidden tmps would stall `start` forever.
    if let Ok(mut rd) = tokio::fs::read_dir(&media_dir).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            if let Some(s) = entry.file_name().to_str() {
                if s.starts_with(".upload-") && s.ends_with(".part") {
                    let _ = tokio::fs::remove_file(entry.path()).await;
                }
            }
        }
    }
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!(">>> beam-media running on http://0.0.0.0:{}", port);
    println!(">>> Media dir: {:?}", media_dir);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
