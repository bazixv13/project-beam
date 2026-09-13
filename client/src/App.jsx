import { useState, useRef, useEffect, lazy, Suspense } from 'react';
import { Send, FileUp, FolderUp, X, Camera, CameraOff, Sun, Moon, LogOut, Copy, Check, Settings, Home } from 'lucide-react';
import { WebRTCConnection } from './webrtc';
import './index.css';

// Lazy load heavy dependencies to minimize critical path JS
const QRScanner = lazy(() => import('./QRScanner'));
const QRCodeDisplay = lazy(() => import('./QRCodeDisplay'));

const dict = {
  en: {
    title: 'Beam',
    connected: 'Connected',
    connecting: 'Connecting...',
    createRoom: 'New Room',
    or: 'or',
    enterCode: 'CODE',
    joinRoom: 'Connect',
    shareCode: 'Room Code',
    copyLink: 'Copy Link',
    copied: 'Copied',
    selectFile: 'File',
    selectFolder: 'Folder',
    orDrop: 'Drop files anywhere',
    sendFile: 'Send',
    resume: 'Resume',
    paused: 'Paused',
    peerLeft: 'Peer disconnected. Waiting to reconnect...',
    cancel: 'Cancel',
    sending: 'Sending',
    receiving: 'Receiving',
    completed: 'Received',
    sent: 'Sent',
    zipping: 'Compressing...',
    scanQr: 'Scan QR',
    noCamera: 'Camera unavailable',
    leaveRoom: 'Leave',
    room: 'Room',
    eta: 'ETA',
    speed: 'Speed',
    noInternet: 'No internet — reconnecting…',
    p2pTitle: 'Direct P2P (WebRTC)',
    p2pDesc: 'Files transfer directly between devices without touching the server. Fully end-to-end encrypted (DTLS).',
    fallbackTitle: 'Relay Fallback (WebSocket)',
    fallbackDesc: 'Direct P2P was blocked by firewalls or NAT. Data streams safely in RAM through the encrypted server and is never stored.',
    handshakingP2p: 'Negotiating direct P2P upgrade…',
    sharedReady: 'Shared file ready — join a room to send it',
    settings: 'Settings',
    transferMode: 'Transfer Mode',
    modeP2P: 'Direct P2P (WebRTC)',
    modeRelay: 'Server Relay (WebSocket)',
    showSvgFrame: 'Show SVG Frame',
    goHome: 'Back to lobby'
  },
  pl: {
    title: 'Beam',
    connected: 'Połączono',
    connecting: 'Łączenie...',
    createRoom: 'Nowy Pokój',
    or: 'lub',
    enterCode: 'KOD',
    joinRoom: 'Połącz',
    shareCode: 'Kod Pokoju',
    copyLink: 'Kopiuj Link',
    copied: 'Skopiowano',
    selectFile: 'Plik',
    selectFolder: 'Folder',
    orDrop: 'Upuść pliki tutaj',
    sendFile: 'Wyślij',
    resume: 'Wznów',
    paused: 'Wstrzymano',
    peerLeft: 'Odbiorca rozłączony. Oczekiwanie na połączenie...',
    cancel: 'Anuluj',
    sending: 'Wysyłanie',
    receiving: 'Pobieranie',
    completed: 'Pobrano',
    sent: 'Wysłano',
    zipping: 'Kompresowanie...',
    scanQr: 'Skanuj QR',
    noCamera: 'Brak kamery',
    leaveRoom: 'Opuść',
    room: 'Pokój',
    eta: 'Pozostało',
    speed: 'Prędkość',
    noInternet: 'Brak internetu — ponawianie…',
    p2pTitle: 'Bezpośrednie P2P (WebRTC)',
    p2pDesc: 'Pliki przesyłane są bezpośrednio między urządzeniami bez udziału serwera. Szyfrowanie end-to-end (DTLS).',
    fallbackTitle: 'Przekaźnik Fallback (WebSocket)',
    fallbackDesc: 'Połączenie P2P zostało zablokowane przez zaporę lub NAT. Dane są bezpiecznie przesyłane w pamięci RAM serwera i nie są zapisywane.',
    handshakingP2p: 'Negocjowanie bezpośredniego P2P…',
    sharedReady: 'Udostępniony plik gotowy — dołącz do pokoju, aby go wysłać',
    settings: 'Ustawienia',
    transferMode: 'Tryb transferu',
    modeP2P: 'Bezpośrednie P2P (WebRTC)',
    modeRelay: 'Przez serwer (WebSocket)',
    showSvgFrame: 'Pokaż ramkę SVG',
    goHome: 'Wróć do lobby'
  }
};

function generate2CharRoomId() {
  const chars = '0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ';
  return chars.charAt(Math.floor(Math.random() * chars.length)) + chars.charAt(Math.floor(Math.random() * chars.length));
}

function formatSize(bytes) {
  if (!bytes || bytes <= 0) return '0 B';
  const mb = bytes / (1024 * 1024);
  if (mb >= 1) return `${mb.toFixed(1)} MB`;
  const kb = bytes / 1024;
  if (kb >= 1) return `${kb.toFixed(0)} KB`;
  return `${bytes} B`;
}

function formatSpeed(bytesPerSec) {
  if (!bytesPerSec || bytesPerSec <= 0) return '';
  const mb = bytesPerSec / (1024 * 1024);
  if (mb >= 1) return `${mb.toFixed(1)} MB/s`;
  const kb = bytesPerSec / 1024;
  return `${kb.toFixed(0)} KB/s`;
}

function formatEta(seconds) {
  if (!seconds || seconds <= 0 || !isFinite(seconds)) return '';
  const s = Math.ceil(seconds);
  if (s < 60) return `${s}s`;
  const mins = Math.floor(s / 60);
  const remSec = s % 60;
  return `${mins}m ${remSec}s`;
}

function getInitialLang() {
  try {
    const saved = localStorage.getItem('lang');
    if (saved && dict[saved]) return saved;
    const langs = (navigator.languages && navigator.languages.length)
      ? navigator.languages
      : [navigator.language || ''];
    for (const l of langs) {
      if (!l) continue;
      const lower = l.toLowerCase();
      if (lower.startsWith('pl')) return 'pl';
      if (lower.startsWith('en')) return 'en';
    }
  } catch (_) {}
  return 'en';
}

function getInitialRoomState() {
  if (typeof window === 'undefined') {
    return { roomId: '', isInitiator: false, connectionState: 'disconnected' };
  }
  try {
    const params = new URLSearchParams(window.location.search);
    const roomParam = params.get('room');
    let savedSession = null;
    try {
      savedSession = JSON.parse(sessionStorage.getItem('p2p_beam_room'));
    } catch (_) {}

    if (roomParam) {
      const targetRoom = roomParam.trim().toUpperCase().replace(/[^0-9A-Z]/g, '').slice(0, 2);
      if (targetRoom) {
        const isInit = (savedSession && savedSession.roomId === targetRoom) ? Boolean(savedSession.isInitiator) : false;
        return { roomId: targetRoom, isInitiator: isInit, connectionState: 'connecting' };
      }
    } else if (savedSession?.roomId) {
      const targetRoom = savedSession.roomId.trim().toUpperCase().replace(/[^0-9A-Z]/g, '').slice(0, 2);
      if (targetRoom) {
        const isInit = Boolean(savedSession.isInitiator);
        return { roomId: targetRoom, isInitiator: isInit, connectionState: 'connecting' };
      }
    }
  } catch (_) {}
  return { roomId: '', isInitiator: false, connectionState: 'disconnected' };
}

const APP_VERSION = 'v1.3.21';

function BrandTitle({ onGoHome, homeLabel }) {
  const [hovered, setHovered] = useState(false);
  const revertTimer = useRef(null);

  const handleMouseEnter = () => {
    setHovered(true);
    if (revertTimer.current) clearTimeout(revertTimer.current);
    revertTimer.current = setTimeout(() => {
      setHovered(false);
    }, 6000);
  };

  const handleMouseLeave = () => {
    if (revertTimer.current) clearTimeout(revertTimer.current);
    setHovered(false);
  };

  useEffect(() => () => { if (revertTimer.current) clearTimeout(revertTimer.current); }, []);

  return (
    <span
      className={`brand-title${hovered ? ' brand-title--version' : ''}`}
      onMouseEnter={handleMouseEnter}
      onMouseLeave={handleMouseLeave}
      onClick={(e) => { e.stopPropagation(); onGoHome?.(); }}
      aria-label={hovered ? APP_VERSION : 'BEAM'}
      style={{ cursor: 'pointer' }}
    >
      <span className="brand-title__beam">BEAM</span>
      <span className="brand-title__version">
        {APP_VERSION}
        <button
          className="brand-home-btn"
          onClick={(e) => { e.stopPropagation(); onGoHome?.(); }}
          title={homeLabel || 'Home'}
          aria-label={homeLabel || 'Home'}
        >
          <Home size={13} />
        </button>
      </span>
    </span>
  );
}

// ============ MORSE CODE EASTER EGG (MOUSE) ============
// Short click (<300ms) = '.' (dit), long press (>=300ms) = '-' (dah).
// Spell P2P in Morse: .--. ..--- .--. → triggers matrix rain effect.
// Visual ripple circles appear at click position as feedback.
const MORSE_TARGET = '.--...---.--.';

function useMorseEasterEgg() {
  const [matrixActive, setMatrixActive] = useState(false);
  const [ripples, setRipples] = useState([]);
  const morseBuffer = useRef('');
  const morseTimeout = useRef(null);
  const mouseDownTime = useRef(0);

  useEffect(() => {
    const handleMouseDown = (e) => {
      // Ignore clicks on inputs, buttons, etc. to avoid interference
      if (['INPUT', 'SELECT', 'TEXTAREA', 'BUTTON', 'A'].includes(e.target.tagName)) return;
      if (e.target.closest('.brand-home-btn') || e.target.closest('.btn-icon') || e.target.closest('button')) return;
      
      mouseDownTime.current = Date.now();
    };

    const handleMouseUp = (e) => {
      if (!mouseDownTime.current) return;
      
      const duration = Date.now() - mouseDownTime.current;
      mouseDownTime.current = 0;

      // Threshold: 250ms. < 250ms is dit (.), >= 250ms is dah (-)
      const char = duration < 250 ? '.' : '-';
      morseBuffer.current += char;

      // Add visual ripple
      const clientX = e.clientX ?? (e.changedTouches ? e.changedTouches[0].clientX : window.innerWidth / 2);
      const clientY = e.clientY ?? (e.changedTouches ? e.changedTouches[0].clientY : 50);

      const newRipple = {
        id: Date.now() + Math.random(),
        x: clientX,
        y: clientY,
        type: char === '.' ? 'dit' : 'dah'
      };
      setRipples(prev => [...prev, newRipple]);
      setTimeout(() => {
        setRipples(prev => prev.filter(r => r.id !== newRipple.id));
      }, 800); // Remove after animation completes

      if (morseTimeout.current) clearTimeout(morseTimeout.current);
      morseTimeout.current = setTimeout(() => { morseBuffer.current = ''; }, 4000);

      // Check if buffer ends with the P2P morse sequence
      const buf = morseBuffer.current;
      if (buf.endsWith(MORSE_TARGET) || buf.endsWith('.--.··---.--.')) {
        morseBuffer.current = '';
        setMatrixActive(true);
        setTimeout(() => setMatrixActive(false), 6000);
      }
    };

    window.addEventListener('mousedown', handleMouseDown);
    window.addEventListener('mouseup', handleMouseUp);
    
    // Also support touch devices
    window.addEventListener('touchstart', handleMouseDown, { passive: true });
    window.addEventListener('touchend', handleMouseUp);

    return () => {
      window.removeEventListener('mousedown', handleMouseDown);
      window.removeEventListener('mouseup', handleMouseUp);
      window.removeEventListener('touchstart', handleMouseDown);
      window.removeEventListener('touchend', handleMouseUp);
      if (morseTimeout.current) clearTimeout(morseTimeout.current);
    };
  }, []);

  return { matrixActive, ripples };
}

// Matrix rain canvas component
function MatrixRain() {
  const canvasRef = useRef(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    let animId;
    let w, h, cols, drops;

    const chars = 'BEAM P2P WEBRTC 01 .--.··---.--.'.split('');

    const resize = () => {
      w = canvas.width = window.innerWidth;
      h = canvas.height = window.innerHeight;
      cols = Math.floor(w / 16);
      drops = new Array(cols).fill(1);
    };
    resize();
    window.addEventListener('resize', resize);

    const draw = () => {
      ctx.fillStyle = 'rgba(0, 0, 0, 0.06)';
      ctx.fillRect(0, 0, w, h);
      ctx.fillStyle = '#a1a1aa'; // Monochrome (grey/white) matrix rain
      ctx.font = '14px monospace';
      for (let i = 0; i < cols; i++) {
        const ch = chars[Math.floor(Math.random() * chars.length)];
        ctx.fillText(ch, i * 16, drops[i] * 16);
        if (drops[i] * 16 > h && Math.random() > 0.975) drops[i] = 0;
        drops[i]++;
      }
      animId = requestAnimationFrame(draw);
    };
    draw();

    return () => {
      cancelAnimationFrame(animId);
      window.removeEventListener('resize', resize);
    };
  }, []);

  return (
    <canvas
      ref={canvasRef}
      className="matrix-rain"
      aria-hidden="true"
    />
  );
}

function App() {
  const [initialRoom] = useState(getInitialRoomState);
  const [roomId, setRoomId] = useState(initialRoom.roomId);
  const [connectionState, setConnectionState] = useState(initialRoom.connectionState);
  const [selectedFile, setSelectedFile] = useState(null);
  const [isZipping, setIsZipping] = useState(false);
  const [zipProgress, setZipProgress] = useState(0);
  const [peerIsZipping, setPeerIsZipping] = useState(false);
  const [isOffline, setIsOffline] = useState(!navigator.onLine);
  const [_isInitiator, setIsInitiator] = useState(initialRoom.isInitiator);
  const [lang, setLang] = useState(getInitialLang);
  const [theme, setTheme] = useState(() => {
    const saved = localStorage.getItem('theme');
    if (saved) return saved;
    return window.matchMedia?.('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  });
  const [showScanner, setShowScanner] = useState(false);
  const [cameraError, setCameraError] = useState(false);
  const [copied, setCopied] = useState(false);
  const [isDragging, setIsDragging] = useState(false);
  const [transferMode, setTransferMode] = useState('p2p'); // 'p2p' | 'fallback'
  const [p2pHandshaking, setP2pHandshaking] = useState(false);
  
  const [showSettingsModal, setShowSettingsModal] = useState(false);
  const [forceRelay, setForceRelay] = useState(() => {
    return localStorage.getItem('forceRelay') === 'true';
  });
  const [showSvgFrame, setShowSvgFrame] = useState(() => {
    return localStorage.getItem('showSvgFrame') !== 'false';
  });

  // Independent send and receive states for concurrent bidirectional transfer
  const [sendProgress, setSendProgress] = useState({
    active: false,
    paused: false,
    completed: false,
    fileName: '',
    fileSize: 0,
    bytesTransferred: 0,
    percent: 0,
    speed: 0,
    eta: 0
  });

  const [receiveProgress, setReceiveProgress] = useState({
    active: false,
    paused: false,
    completed: false,
    fileName: '',
    fileSize: 0,
    bytesTransferred: 0,
    percent: 0,
    speed: 0,
    eta: 0
  });

  const webrtc = useRef(null);
  const sendTimerRef = useRef(null);
  const receiveTimerRef = useRef(null);
  const wakeLockRef = useRef(null);

  const t = dict[lang] || dict.en;

  useEffect(() => {
    try {
      localStorage.setItem('lang', lang);
      document.documentElement.setAttribute('lang', lang);
    } catch (_) {}
  }, [lang]);

  useEffect(() => {
    document.documentElement.setAttribute('data-theme', theme);
    localStorage.setItem('theme', theme);
  }, [theme]);

  useEffect(() => {
    localStorage.setItem('forceRelay', forceRelay);
    if (forceRelay && transferMode === 'p2p') {
      // Re-init WebRTC to force relay if changed mid-session, though normally users change this before connecting
    }
  }, [forceRelay]);

  useEffect(() => {
    localStorage.setItem('showSvgFrame', showSvgFrame);
  }, [showSvgFrame]);

  useEffect(() => {
    const handleUnload = () => {
      if (webrtc.current) {
        webrtc.current.close();
      }
    };
    window.addEventListener('beforeunload', handleUnload);
    window.addEventListener('pagehide', handleUnload);
    return () => {
      window.removeEventListener('beforeunload', handleUnload);
      window.removeEventListener('pagehide', handleUnload);
    };
  }, []);

  // Keep the screen (and its timers/sockets) alive while connected.
  // Helps against screen-off suspends on phones; the peer-busy signal
  // covers the native file picker overlay itself.
  useEffect(() => {
    if (connectionState !== 'connected') return;
    if (typeof navigator === 'undefined' || !('wakeLock' in navigator)) return;
    let lock = null;
    let cancelled = false;
    const request = async () => {
      try {
        const sentinel = await navigator.wakeLock.request('screen');
        if (cancelled) {
          try { await sentinel.release(); } catch (_) {}
          return;
        }
        lock = sentinel;
        wakeLockRef.current = sentinel;
      } catch (_) {}
    };
    request();
    const reRequest = () => {
      if (document.visibilityState === 'visible' && !cancelled) request();
    };
    document.addEventListener('visibilitychange', reRequest);
    return () => {
      cancelled = true;
      document.removeEventListener('visibilitychange', reRequest);
      if (lock) {
        try { lock.release(); } catch (_) {}
      }
      wakeLockRef.current = null;
    };
  }, [connectionState]);

  const { matrixActive, ripples } = useMorseEasterEgg();

  const toggleTheme = () => {
    setTheme(prev => (prev === 'dark' ? 'light' : 'dark'));
  };

  const initWebRTC = (id, isInit) => {
    if (webrtc.current) {
      webrtc.current.close();
    }
    setConnectionState('connecting');

    webrtc.current = new WebRTCConnection({
      roomId: id,
      isInitiator: isInit,
      forceRelay: forceRelay,
      onHandshakeStateChange: (isHandshaking) => {
        setP2pHandshaking(isHandshaking);
      },
      onConnectionStateChange: (state, mode) => {
        if (state === 'connected') {
          setConnectionState('connected');
          if (mode) setTransferMode(mode === 'ws' ? 'fallback' : 'p2p');
        }
        if (state === 'connecting') {
          setConnectionState('connecting');
          // Preserve selectedFile so user can resume sending once reconnected!
          setSendProgress(prev => ({
            ...prev,
            active: false,
            paused: prev.bytesTransferred > 0 && !prev.completed
          }));
          setReceiveProgress(prev => ({
            ...prev,
            active: false,
            paused: prev.bytesTransferred > 0 && !prev.completed
          }));
        }
        if (state === 'disconnected' || state === 'failed') {
          setConnectionState('disconnected');
          setTransferMode('p2p');
        }
      },
      onSendProgress: (progress) => {
        if (progress.active) {
          setConnectionState('connected');
        }
        setSendProgress(progress);
        if (progress.completed) {
          if (sendTimerRef.current) clearTimeout(sendTimerRef.current);
          sendTimerRef.current = setTimeout(() => {
            setSendProgress({
              active: false,
              paused: false,
              completed: false,
              fileName: '',
              fileSize: 0,
              bytesTransferred: 0,
              percent: 0,
              speed: 0,
              eta: 0
            });
            setSelectedFile(null);
            const fileInput = document.getElementById('fileInput');
            const folderInput = document.getElementById('folderInput');
            if (fileInput) fileInput.value = '';
            if (folderInput) folderInput.value = '';
          }, 3500);
        }
      },
      onReceiveProgress: (progress) => {
        if (progress.active) {
          setConnectionState('connected');
        }
        setReceiveProgress(progress);
        if (progress.completed) {
          if (receiveTimerRef.current) clearTimeout(receiveTimerRef.current);
          receiveTimerRef.current = setTimeout(() => {
            setReceiveProgress({
              active: false,
              paused: false,
              completed: false,
              fileName: '',
              fileSize: 0,
              bytesTransferred: 0,
              percent: 0,
              speed: 0,
              eta: 0
            });
          }, 4000);
        }
      },
      onFileReceived: (blob, name) => {
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = name;
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
        setTimeout(() => URL.revokeObjectURL(url), 60000);
      },
      onPeerZipping: (active) => {
        setPeerIsZipping(active);
      },
      onOffline: (offline) => {
        setIsOffline(offline);
      }
    });
  };

  useEffect(() => {
    if (initialRoom.roomId) {
      if (!window.location.search.includes(`room=${initialRoom.roomId}`)) {
        window.history.replaceState({ room: initialRoom.roomId }, '', `${window.location.pathname}?room=${initialRoom.roomId}`);
      }
      sessionStorage.setItem('p2p_beam_room', JSON.stringify({ roomId: initialRoom.roomId, isInitiator: initialRoom.isInitiator }));
      initWebRTC(initialRoom.roomId, initialRoom.isInitiator);
    }

    const handleOffline = () => setIsOffline(true);
    const handleOnline = () => {
      setIsOffline(false);
      webrtc.current?.reconnect();
    };
    window.addEventListener('offline', handleOffline);
    window.addEventListener('online', handleOnline);

    return () => {
      if (sendTimerRef.current) clearTimeout(sendTimerRef.current);
      if (receiveTimerRef.current) clearTimeout(receiveTimerRef.current);
      if (webrtc.current) webrtc.current.close();
      window.removeEventListener('offline', handleOffline);
      window.removeEventListener('online', handleOnline);
    };
  }, []);

  const handleCreateRoom = () => {
    const newRoomId = generate2CharRoomId();
    setRoomId(newRoomId);
    setIsInitiator(true);
    sessionStorage.setItem('p2p_beam_room', JSON.stringify({ roomId: newRoomId, isInitiator: true }));
    window.history.replaceState({ room: newRoomId }, '', `${window.location.pathname}?room=${newRoomId}`);
    initWebRTC(newRoomId, true);
  };

  const handleJoinRoom = (targetRoom) => {
    const codeToUse = typeof targetRoom === 'string' ? targetRoom : roomId;
    if (!codeToUse) return;
    const cleanRoom = codeToUse.trim().toUpperCase().replace(/[^0-9A-Z]/g, '').slice(0, 2);
    if (cleanRoom.length !== 2) return;
    setIsInitiator(false);
    sessionStorage.setItem('p2p_beam_room', JSON.stringify({ roomId: cleanRoom, isInitiator: false }));
    window.history.replaceState({ room: cleanRoom }, '', `${window.location.pathname}?room=${cleanRoom}`);
    initWebRTC(cleanRoom, false);
  };

  const handleScan = (decodedText) => {
    setShowScanner(false);
    let extracted = decodedText;
    try {
      extracted = new URL(decodedText).searchParams.get('room') ?? decodedText;
    } catch (_) {}
    const finalRoom = extracted.trim().toUpperCase().replace(/[^0-9A-Z]/g, '').slice(0, 2);
    if (!finalRoom) return;
    setRoomId(finalRoom);
    setIsInitiator(false);
    sessionStorage.setItem('p2p_beam_room', JSON.stringify({ roomId: finalRoom, isInitiator: false }));
    window.history.replaceState({ room: finalRoom }, '', `${window.location.pathname}?room=${finalRoom}`);
    initWebRTC(finalRoom, false);
  };

  const handleLeaveRoom = () => {
    if (sendTimerRef.current) clearTimeout(sendTimerRef.current);
    if (receiveTimerRef.current) clearTimeout(receiveTimerRef.current);
    if (webrtc.current) {
      webrtc.current.close();
      webrtc.current = null;
    }
    sessionStorage.removeItem('p2p_beam_room');
    window.history.replaceState({}, '', window.location.pathname);
    setRoomId('');
    setIsInitiator(false);
    setConnectionState('disconnected');
    setSelectedFile(null);
    setTransferMode('p2p');
    setSendProgress({ active: false, completed: false, fileName: '', fileSize: 0, bytesTransferred: 0, percent: 0, speed: 0, eta: 0 });
    setReceiveProgress({ active: false, completed: false, fileName: '', fileSize: 0, bytesTransferred: 0, percent: 0, speed: 0, eta: 0 });
  };

  const zipFilesAndSetState = async (fileEntries, defaultZipName) => {
    setIsZipping(true);
    setZipProgress(0);

    // Notify connected peer we are about to compress
    if (webrtc.current) {
      webrtc.current.sendControl('zip-start');
    }

    try {
      const JSZipModule = await import('jszip');
      const JSZip = JSZipModule.default || JSZipModule;
      const zip = new JSZip();

      // Pass native File/Blob objects directly without buffer preloading (avoids OOM on large folders)
      for (const { path, file } of fileEntries) {
        zip.file(path, file);
      }

      const content = await zip.generateAsync(
        { type: 'blob', compression: 'STORE' },
        (metadata) => setZipProgress(Math.floor(metadata.percent))
      );

      const zipFile = new File([content], defaultZipName, { type: 'application/zip' });
      setSelectedFile(zipFile);
    } catch (err) {
      console.error('Zip compression error:', err);
    } finally {
      setIsZipping(false);
      setZipProgress(0);
      if (webrtc.current) {
        webrtc.current.sendControl('zip-done');
      }
    }
  };

  const handleFileSelect = async (e) => {
    closeFilePicker();
    const fileList = e.target.files;
    if (!fileList || fileList.length === 0) return;
    setSendProgress({
      active: false,
      paused: false,
      completed: false,
      fileName: '',
      fileSize: 0,
      bytesTransferred: 0,
      percent: 0,
      speed: 0,
      eta: 0
    });
    if (fileList.length === 1) {
      const file = fileList[0];
      e.target.value = '';
      setSelectedFile(file);
    } else {
      const entries = [];
      for (let i = 0; i < fileList.length; i++) {
        entries.push({ file: fileList[i], path: fileList[i].name });
      }
      e.target.value = '';
      zipFilesAndSetState(entries, 'files.zip');
    }
  };

  const handleFolderSelect = async (e) => {
    closeFilePicker();
    const fileList = e.target.files;
    if (!fileList || fileList.length === 0) return;
    setSendProgress({
      active: false,
      paused: false,
      completed: false,
      fileName: '',
      fileSize: 0,
      bytesTransferred: 0,
      percent: 0,
      speed: 0,
      eta: 0
    });
    let folderName = 'folder.zip';
    const entries = [];
    for (let i = 0; i < fileList.length; i++) {
      const file = fileList[i];
      const path = file.webkitRelativePath || file.name;
      if (i === 0 && file.webkitRelativePath) {
        folderName = file.webkitRelativePath.split('/')[0] + '.zip';
      }
      entries.push({ file, path });
    }
    e.target.value = '';
    zipFilesAndSetState(entries, folderName);
  };

  const handleSendFile = () => {
    if (selectedFile && webrtc.current) {
      webrtc.current.sendFile(selectedFile);
    }
  };

  // Announce the native file picker to the peer *before* it opens so the
  // peer extends our silence allowance while OS picker freezes our timers.
  // closeFilePicker runs on change, cancel, and window-focus fallback.
  const openFilePicker = (inputId) => {
    if (webrtc.current) webrtc.current.notifyPickerOpen();
    const onFocusBack = () => {
      window.removeEventListener('focus', onFocusBack);
      setTimeout(() => {
        if (webrtc.current) webrtc.current.notifyPickerClosed();
      }, 500);
    };
    window.addEventListener('focus', onFocusBack);
    setTimeout(() => window.removeEventListener('focus', onFocusBack), 180000);
    const el = document.getElementById(inputId);
    if (el) {
      el.click();
    } else {
      if (webrtc.current) webrtc.current.notifyPickerClosed();
    }
  };

  const closeFilePicker = () => {
    if (webrtc.current) webrtc.current.notifyPickerClosed();
  };

  const handleCancelFile = () => {
    if (webrtc.current) {
      webrtc.current.cancelSend();
    }
    setSelectedFile(null);
    setSendProgress({
      active: false,
      paused: false,
      completed: false,
      fileName: '',
      fileSize: 0,
      bytesTransferred: 0,
      percent: 0,
      speed: 0,
      eta: 0
    });
    const fileInput = document.getElementById('fileInput');
    const folderInput = document.getElementById('folderInput');
    if (fileInput) fileInput.value = '';
    if (folderInput) folderInput.value = '';
  };

  const generateRoomUrl = () => {
    return `${window.location.origin}${window.location.pathname}?room=${roomId}`;
  };

  const handleCopyLink = () => {
    const url = generateRoomUrl();
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(url).then(() => {
        setCopied(true);
        setTimeout(() => setCopied(false), 2000);
      }).catch(() => {});
    }
  };

  // Recursively traverses dropped directories via webkitGetAsEntry
  const scanDroppedEntries = async (dataTransfer) => {
    const items = dataTransfer.items;
    const results = [];

    if (items && items.length > 0 && items[0].webkitGetAsEntry) {
      const queue = [];
      for (let i = 0; i < items.length; i++) {
        const entry = items[i].webkitGetAsEntry();
        if (entry) queue.push({ entry, path: '' });
      }

      while (queue.length > 0) {
        const { entry, path } = queue.shift();
        if (entry.isFile) {
          const file = await new Promise((resolve) => entry.file(resolve, () => resolve(null)));
          if (file) {
            const relPath = path ? `${path}/${file.name}` : file.name;
            results.push({ file, path: relPath, isFromFolder: Boolean(path) });
          }
        } else if (entry.isDirectory) {
          const dirReader = entry.createReader();
          const dirPath = path ? `${path}/${entry.name}` : entry.name;
          const subEntries = await new Promise((resolve) => {
            const all = [];
            function readNextBatch() {
              dirReader.readEntries((batch) => {
                if (!batch || batch.length === 0) {
                  resolve(all);
                } else {
                  all.push(...batch);
                  readNextBatch();
                }
              }, () => resolve(all));
            }
            readNextBatch();
          });

          for (const sub of subEntries) {
            queue.push({ entry: sub, path: dirPath });
          }
        }
      }
    } else if (dataTransfer.files && dataTransfer.files.length > 0) {
      for (let i = 0; i < dataTransfer.files.length; i++) {
        const file = dataTransfer.files[i];
        results.push({ file, path: file.webkitRelativePath || file.name, isFromFolder: Boolean(file.webkitRelativePath) });
      }
    }

    return results;
  };

  const handleDrop = async (e) => {
    const entries = await scanDroppedEntries(e.dataTransfer);
    if (!entries || entries.length === 0) return;

    setSendProgress({
      active: false,
      paused: false,
      completed: false,
      fileName: '',
      fileSize: 0,
      bytesTransferred: 0,
      percent: 0,
      speed: 0,
      eta: 0
    });

    if (entries.length === 1 && !entries[0].isFromFolder) {
      setSelectedFile(entries[0].file);
    } else {
      let zipName = 'dropped-files.zip';
      if (entries[0].path.includes('/')) {
        zipName = entries[0].path.split('/')[0] + '.zip';
      }
      zipFilesAndSetState(entries, zipName);
    }
  };

  const inRoom = connectionState !== 'disconnected';

  // Web Share Target: pick up files shared from the Android share sheet.
  // The service worker stashes them in IndexedDB ('beam-share') and redirects
  // here with ?share-target. We stage them on the home screen so the user
  // just creates/joins a room and hits Send. Runs once on mount.
  useEffect(() => {
    if (!window.location.search.includes('share-target')) return;
    // Strip the marker immediately so refresh/back doesn't re-trigger.
    try {
      const params = new URLSearchParams(window.location.search);
      params.delete('share-target');
      const rest = params.toString();
      window.history.replaceState({}, '', window.location.pathname + (rest ? `?${rest}` : ''));
    } catch (_) {}

    let cancelled = false;
    (async () => {
      try {
        const files = await new Promise((resolve, reject) => {
          const req = indexedDB.open('beam-share', 1);
          req.onupgradeneeded = () => req.result.createObjectStore('files');
          req.onsuccess = () => {
            const db = req.result;
            const tx = db.transaction('files', 'readwrite');
            const store = tx.objectStore('files');
            const get = store.get('shared-files');
            get.onsuccess = () => {
              store.delete('shared-files');
              resolve(get.result || []);
            };
            get.onerror = () => reject(get.error);
            tx.oncomplete = () => db.close();
          };
          req.onerror = () => reject(req.error);
        });
        if (cancelled || !files || files.length === 0) return;
        const valid = files.filter((f) => f && typeof f.name === 'string' && f.size > 0);
        if (valid.length === 0) return;
        setSendProgress({
          active: false,
          paused: false,
          completed: false,
          fileName: '',
          fileSize: 0,
          bytesTransferred: 0,
          percent: 0,
          speed: 0,
          eta: 0
        });
        if (valid.length === 1) {
          setSelectedFile(valid[0]);
        } else {
          zipFilesAndSetState(valid.map((file) => ({ file, path: file.name })), 'shared-files.zip');
        }
      } catch (_) {}
    })();
    return () => { cancelled = true; };
  }, []);

  return (
    <div 
      className={`app-shell ${isDragging ? 'is-dragging' : ''}`}
      onDragOver={(e) => { e.preventDefault(); setIsDragging(true); }}
      onDragLeave={() => setIsDragging(false)}
      onDrop={(e) => {
        e.preventDefault();
        setIsDragging(false);
        if (inRoom) handleDrop(e);
      }}
    >
      {/* Morse Easter Egg: Matrix Rain */}
      {matrixActive && <MatrixRain />}
      
      {/* Morse Easter Egg: Ripples */}
      {ripples.map(r => (
        <div 
          key={r.id} 
          className={`morse-ripple ${r.type}`} 
          style={{ left: r.x, top: r.y }} 
        />
      ))}

      {showScanner && (
        <Suspense fallback={null}>
          <QRScanner 
            onScan={handleScan} 
            onClose={() => setShowScanner(false)} 
            onPermissionDenied={() => setCameraError(true)}
          />
        </Suspense>
      )}

      {/* Brutalist Top Header */}
      <header className="app-header">
        <div className="brand">
          <BrandTitle onGoHome={inRoom ? handleLeaveRoom : () => window.location.reload()} homeLabel={t.goHome} />
          {inRoom && connectionState === 'connected' && (
            <div className={`mode-badge-wrapper ${transferMode}`} tabIndex={0}>
              <span className="mode-indicator">
                {transferMode === 'p2p' ? 'P2P' : 'RELAY'}
                {transferMode === 'fallback' && p2pHandshaking && (
                  <span className="handshake-loader" title={t.handshakingP2p} />
                )}
              </span>
              <div className="mode-tooltip" role="tooltip">
                <div className="tooltip-header">
                  <span className="tooltip-tag">{transferMode === 'p2p' ? 'E2EE' : 'TLS'}</span>
                  <span className="tooltip-title">
                    {transferMode === 'p2p' ? t.p2pTitle : t.fallbackTitle}
                  </span>
                </div>
                <p className="tooltip-desc">
                  {transferMode === 'p2p' ? t.p2pDesc : t.fallbackDesc}
                </p>
                {transferMode === 'fallback' && p2pHandshaking && (
                  <p className="tooltip-handshake">
                    <span className="handshake-spinner" />
                    {t.handshakingP2p}
                  </p>
                )}
              </div>
            </div>
          )}
        </div>

        <div className="header-actions">
          {inRoom && (
            <div className="room-indicator">
              <span className="room-badge" onClick={handleCopyLink} title="Click to copy link">
                {roomId}
              </span>
              <button 
                className="btn-icon" 
                onClick={handleLeaveRoom}
                title={t.leaveRoom}
                aria-label={t.leaveRoom}
              >
                <LogOut size={16} />
              </button>
            </div>
          )}

          <button 
            className="btn-icon" 
            onClick={toggleTheme}
            title={theme === 'dark' ? 'Light' : 'Dark'}
            aria-label="Toggle theme"
          >
            {theme === 'dark' ? <Sun size={16} /> : <Moon size={16} />}
          </button>

          <button 
            className="btn-icon lang-toggle" 
            onClick={() => setLang(lang === 'pl' ? 'en' : 'pl')} 
            title="Switch Language"
          >
            <span>{lang.toUpperCase()}</span>
          </button>

          <button 
            className="btn-icon" 
            onClick={() => setShowSettingsModal(true)}
            title={t.settings}
            aria-label={t.settings}
          >
            <Settings size={16} />
          </button>
        </div>
      </header>

      {/* Settings Modal */}
      {showSettingsModal && (
        <div className="settings-modal-overlay" onClick={() => setShowSettingsModal(false)}>
          <div className="settings-modal" onClick={e => e.stopPropagation()}>
            <div className="settings-header">
              <h3>{t.settings}</h3>
              <button className="btn-icon" onClick={() => setShowSettingsModal(false)}>
                <X size={18} />
              </button>
            </div>
            
            <div className="settings-body">
              <div className="setting-item">
                <div className="setting-info">
                  <span>{t.transferMode}</span>
                </div>
                <select 
                  className="setting-select" 
                  value={forceRelay ? 'relay' : 'p2p'} 
                  onChange={e => {
                    setForceRelay(e.target.value === 'relay');
                  }}
                >
                  <option value="p2p">{t.modeP2P}</option>
                  <option value="relay">{t.modeRelay}</option>
                </select>
              </div>

              <div className="setting-item">
                <div className="setting-info">
                  <span>{t.showSvgFrame}</span>
                </div>
                <label className="toggle-switch">
                  <input 
                    type="checkbox" 
                    checked={showSvgFrame} 
                    onChange={e => setShowSvgFrame(e.target.checked)} 
                  />
                  <span className="slider"></span>
                </label>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Main Single-Surface Canvas */}
      <main className="main-content">
        
        {/* Disconnected: Simple clean 2-char code connection */}
        {connectionState === 'disconnected' && (
          <div className="view-flow">
            {/* File shared from the Android share sheet — staged, waiting for a room */}
            {selectedFile && !sendProgress.active && !isZipping && (
              <div className="staged-file">
                <div className="staged-details">
                  <div className="staged-title-row">
                    <p className="staged-name">{selectedFile.name}</p>
                  </div>
                  <p className="staged-meta">
                    {formatSize(selectedFile.size)} • {t.sharedReady}
                  </p>
                </div>
                <div className="staged-buttons">
                  <button className="btn-icon" onClick={handleCancelFile} title={t.cancel}>
                    <X size={18} />
                  </button>
                </div>
              </div>
            )}

            {/* Shared multi-file zip being compressed */}
            {isZipping && (
              <div className="transfer-strip zipping">
                <div className="strip-info">
                  <span className="strip-type">{t.zipping}</span>
                  <span className="strip-title">{zipProgress}%</span>
                </div>
                <div className="meter-track">
                  <div className="meter-fill" style={{ width: `${zipProgress}%` }} />
                </div>
              </div>
            )}

            <button onClick={handleCreateRoom} className="btn-solid">
              <span>{t.createRoom}</span>
            </button>

            <div className="divider">
              <span>{t.or}</span>
            </div>

            <div className="join-form">
              <div className="input-group">
                <input 
                  type="text" 
                  placeholder="--"
                  value={roomId}
                  maxLength={2}
                  onChange={(e) => {
                    const val = e.target.value.toUpperCase().replace(/[^0-9A-Z]/g, '').slice(0, 2);
                    setRoomId(val);
                    if (val.length === 2) {
                      handleJoinRoom(val);
                    }
                  }}
                  onKeyDown={(e) => { if (e.key === 'Enter') handleJoinRoom(); }}
                  className="code-input"
                  autoCapitalize="characters"
                  autoComplete="off"
                  spellCheck={false}
                  autoFocus
                />
                <button 
                  type="button"
                  className="camera-btn" 
                  onClick={() => { if(!cameraError) setShowScanner(true); }}
                  title={cameraError ? t.noCamera : t.scanQr}
                >
                  {cameraError ? <CameraOff size={18} /> : <Camera size={18} />}
                </button>
              </div>
            </div>
          </div>
        )}

        {/* Connecting: Share 2-Char Room Code + QR */}
        {connectionState === 'connecting' && (
          <div className="view-flow connect-flow">
            {showSvgFrame && _isInitiator ? (
              <div className="svg-frame-wrapper">
                <svg className="room-svg-bg" width="407" height="458" viewBox="0 0 407 458" fill="none" xmlns="http://www.w3.org/2000/svg">
                  <path d="M82.543 33H58.5L91.5 0H115.543L82.543 33Z" fill="var(--text-main)"/>
                  <path d="M113.543 33H89.5L122.5 0H146.543L113.543 33Z" fill="var(--text-main)"/>
                  <path d="M51.5322 33H27.5L60.4756 0H84.5068L51.5322 33Z" fill="var(--text-main)"/>
                  <path d="M304.29 446H232.315L239.315 439H311.29L304.29 446Z" fill="var(--text-main)"/>
                  <path d="M407 37.9648V421.035L374.535 453.5H224.815L229.815 448.5H372.465L402 418.965V40.0352L367.465 5.5H154.035L118.035 41.5H24.0352L5 60.5352V411.965L45.5352 452.5H189.465L210.965 431H319.291L314.291 436H213.035L191.535 457.5H43.4648L0 414.035V58.4648L21.9648 36.5H115.965L151.965 0.5H369.535L407 37.9648Z" fill="var(--text-main)"/>
                  <rect x="244.107" y="437" width="1.94331" height="13.9629" transform="rotate(45 244.107 437)" fill="var(--bg-app)"/>
                  <rect x="249.689" y="436.684" width="1.94331" height="14.5882" transform="rotate(45 249.689 436.684)" fill="var(--bg-app)"/>
                  <rect x="254.373" y="437" width="1.94331" height="13.9629" transform="rotate(45 254.373 437)" fill="var(--bg-app)"/>
                  <rect x="259.909" y="436.731" width="1.94331" height="14.183" transform="rotate(45 259.909 436.731)" fill="var(--bg-app)"/>
                  <rect x="264.373" y="437.316" width="1.94331" height="13.9629" transform="rotate(45 264.373 437.316)" fill="var(--bg-app)"/>
                  <rect x="269.956" y="437" width="1.94331" height="14.5882" transform="rotate(45 269.956 437)" fill="var(--bg-app)"/>
                  <rect x="274.64" y="437.316" width="1.94331" height="13.9629" transform="rotate(45 274.64 437.316)" fill="var(--bg-app)"/>
                  <rect x="280.175" y="437.047" width="1.94331" height="14.183" transform="rotate(45 280.175 437.047)" fill="var(--bg-app)"/>
                  <rect x="285.373" y="437.316" width="1.94331" height="13.9629" transform="rotate(45 285.373 437.316)" fill="var(--bg-app)"/>
                  <rect x="290.956" y="437" width="1.94331" height="14.5882" transform="rotate(45 290.956 437)" fill="var(--bg-app)"/>
                  <rect x="295.64" y="437.316" width="1.94331" height="13.9629" transform="rotate(45 295.64 437.316)" fill="var(--bg-app)"/>
                  <rect x="301.175" y="437.047" width="1.94331" height="14.183" transform="rotate(45 301.175 437.047)" fill="var(--bg-app)"/>
                  <rect x="306.373" y="437.316" width="1.94331" height="13.9629" transform="rotate(45 306.373 437.316)" fill="var(--bg-app)"/>
                  <rect x="311.956" y="437" width="1.94331" height="14.5882" transform="rotate(45 311.956 437)" fill="var(--bg-app)"/>
                  <rect x="316.64" y="437.316" width="1.94331" height="13.9629" transform="rotate(45 316.64 437.316)" fill="var(--bg-app)"/>
                  <rect x="322.175" y="437.047" width="1.94331" height="14.183" transform="rotate(45 322.175 437.047)" fill="var(--bg-app)"/>
                </svg>
                <div className="frame-content">
                  <div className="code-card">
                    <span className="code-label">{t.shareCode}</span>
                    <div className="code-huge" onClick={handleCopyLink}>
                      {roomId}
                    </div>
                    <button className="btn-copy" onClick={handleCopyLink}>
                      {copied ? <Check size={14} /> : <Copy size={14} />}
                      <span>{copied ? t.copied : t.copyLink}</span>
                    </button>
                  </div>
                  <div className="qr-wrapper">
                    <Suspense fallback={<div style={{ width: 140, height: 140 }} />}>
                      <QRCodeDisplay value={generateRoomUrl()} size={140} />
                    </Suspense>
                  </div>
                </div>
              </div>
            ) : (
              <>
                <div className="code-card">
                  <span className="code-label">{t.shareCode}</span>
                  <div className="code-huge" onClick={handleCopyLink}>
                    {roomId}
                  </div>
                  <button className="btn-copy" onClick={handleCopyLink}>
                    {copied ? <Check size={14} /> : <Copy size={14} />}
                    <span>{copied ? t.copied : t.copyLink}</span>
                  </button>
                </div>
                <div className="qr-wrapper">
                  <Suspense fallback={<div style={{ width: 140, height: 140 }} />}>
                    <QRCodeDisplay value={generateRoomUrl()} size={140} />
                  </Suspense>
                </div>
              </>
            )}

            <div className="status-indicator pulse">
              <span className="dot"></span>
              <span>{sendProgress.paused || receiveProgress.paused ? t.peerLeft : t.connecting}</span>
            </div>
          </div>
        )}

        {/* Connected: Full-Featured Bidirectional Transfer Workspace */}
        {connectionState === 'connected' && (
          <div className="view-flow transfer-flow">

            {/* Incoming Receiving Transfer (Concurrent) */}
            {(receiveProgress.active || receiveProgress.completed) && (
              <div className={`transfer-strip receiving ${receiveProgress.completed ? 'is-complete' : ''}`}>
                <div className="strip-info">
                  <span className="strip-type">{receiveProgress.completed ? t.completed : t.receiving}</span>
                  <span className="strip-title" title={receiveProgress.fileName}>{receiveProgress.fileName}</span>
                </div>

                <div className="strip-metrics">
                  <span>{formatSize(receiveProgress.bytesTransferred)} / {formatSize(receiveProgress.fileSize)}</span>
                  {receiveProgress.active && receiveProgress.speed > 0 && (
                    <span>• {formatSpeed(receiveProgress.speed)}</span>
                  )}
                  {receiveProgress.active && receiveProgress.eta > 0 && (
                    <span>• {formatEta(receiveProgress.eta)}</span>
                  )}
                </div>

                <div className="meter-track">
                  <div 
                    className="meter-fill" 
                    style={{ width: `${receiveProgress.percent}%` }}
                  />
                </div>
              </div>
            )}

            {/* Peer Compressing Indicator (shown on receiver side) */}
            {peerIsZipping && !receiveProgress.active && (
              <div className="transfer-strip zipping">
                <div className="strip-info">
                  <span className="strip-type">{t.zipping}</span>
                  <span className="strip-title">…</span>
                </div>
                <div className="meter-track">
                  <div className="meter-fill" style={{ width: '100%', animation: 'pulse-bar 1.4s ease-in-out infinite' }} />
                </div>
              </div>
            )}

            {/* Outgoing Sending Transfer (Concurrent) */}
            {(sendProgress.active || sendProgress.completed) && (
              <div className={`transfer-strip sending ${sendProgress.completed ? 'is-complete' : ''}`}>
                <div className="strip-info">
                  <span className="strip-type">{sendProgress.completed ? t.sent : t.sending}</span>
                  <span className="strip-title" title={sendProgress.fileName}>{sendProgress.fileName}</span>
                </div>

                <div className="strip-metrics">
                  <span>{formatSize(sendProgress.bytesTransferred)} / {formatSize(sendProgress.fileSize)}</span>
                  {sendProgress.active && sendProgress.speed > 0 && (
                    <span>• {formatSpeed(sendProgress.speed)}</span>
                  )}
                  {sendProgress.active && sendProgress.eta > 0 && (
                    <span>• {formatEta(sendProgress.eta)}</span>
                  )}
                </div>

                <div className="meter-track">
                  <div 
                    className="meter-fill" 
                    style={{ width: `${sendProgress.percent}%` }}
                  />
                </div>
              </div>
            )}

            {/* Zipping Indicator */}
            {isZipping && (
              <div className="transfer-strip zipping">
                <div className="strip-info">
                  <span className="strip-type">{t.zipping}</span>
                  <span className="strip-title">{zipProgress}%</span>
                </div>
                <div className="meter-track">
                  <div className="meter-fill" style={{ width: `${zipProgress}%` }} />
                </div>
              </div>
            )}

            {/* Pending File Ready to Send or Resume */}
            {selectedFile && !sendProgress.active && !isZipping && (
              <div className="staged-file">
                <div className="staged-details">
                  <div className="staged-title-row">
                    <p className="staged-name">{selectedFile.name}</p>
                    {sendProgress.paused && (
                      <span className="paused-badge">{t.paused} ({sendProgress.percent}%)</span>
                    )}
                  </div>
                  <p className="staged-meta">
                    {sendProgress.paused 
                      ? `${formatSize(sendProgress.bytesTransferred)} / ${formatSize(selectedFile.size)}`
                      : formatSize(selectedFile.size)
                    }
                  </p>
                </div>
                <div className="staged-buttons">
                  <button className="btn-solid btn-compact" onClick={handleSendFile}>
                    <Send size={15} />
                    <span>{sendProgress.paused ? t.resume : t.sendFile}</span>
                  </button>
                  <button className="btn-icon" onClick={handleCancelFile} title={t.cancel}>
                    <X size={18} />
                  </button>
                </div>
              </div>
            )}

            {/* Drop / File Select Area (Always Available for Instant Sending) */}
            {!selectedFile && !sendProgress.active && !isZipping && (
              <div className="drop-canvas">
                <div className="file-triggers">
                  <button 
                    type="button" 
                    className="btn-trigger"
                    onClick={() => openFilePicker('fileInput')}
                  >
                    <FileUp size={22} />
                    <span>{t.selectFile}</span>
                  </button>

                  <div className="trigger-sep" />

                  <button 
                    type="button" 
                    className="btn-trigger"
                    onClick={() => openFilePicker('folderInput')}
                  >
                    <FolderUp size={22} />
                    <span>{t.selectFolder}</span>
                  </button>
                </div>

                <p className="canvas-hint">{t.orDrop}</p>
              </div>
            )}

          </div>
        )}
      </main>
      {/*
        Hidden file inputs live at the App root, outside every
        connection-state conditional. Unmounting an <input type=file>
        while its native picker dialog is open cancels the dialog,
        which is exactly what used to eat the selection whenever the
        socket blipped during picking on phones.
      */}
      <input
        id="fileInput"
        type="file"
        multiple
        style={{ display: 'none' }}
        onChange={handleFileSelect}
        onCancel={closeFilePicker}
      />
      <input
        id="folderInput"
        type="file"
        webkitdirectory="true"
        directory="true"
        style={{ display: 'none' }}
        onChange={handleFolderSelect}
        onCancel={closeFilePicker}
      />
    </div>
  );
}

export default App;

