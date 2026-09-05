const CHUNK_SIZE = 65536; // 64KB universal optimal SCTP chunk size
const BUFFER_HIGH = 2 * 1024 * 1024; // 2MB high watermark for maximum pipelining
const BUFFER_LOW = 256 * 1024; // 256KB low watermark
const BLOCK_SIZE = 4 * 1024 * 1024; // 4MB read blocks
const BATCH_FLUSH_SIZE = 16 * 1024 * 1024; // 16MB Blob flush

const ICE_SERVERS = [
  {
    urls: [
      'stun:stun.l.google.com:19302',
      'stun:stun1.l.google.com:19302',
      'stun:stun2.l.google.com:19302',
      'stun:stun3.l.google.com:19302',
      'stun:stun4.l.google.com:19302',
      'stun:stun.cloudflare.com:3478',
      'stun:stun.services.mozilla.com'
    ]
  },
  {
    urls: 'turn:openrelay.metered.ca:80',
    username: 'openrelayproject',
    credential: 'openrelayproject'
  },
  {
    urls: 'turn:openrelay.metered.ca:443',
    username: 'openrelayproject',
    credential: 'openrelayproject'
  }
];

export class WebRTCConnection {
  constructor({
    roomId,
    isInitiator,
    onConnectionStateChange,
    onSendProgress,
    onReceiveProgress,
    onFileReceived,
    onPeerZipping,
    onOffline,
    onHandshakeStateChange
  }) {
    this.roomId = roomId;
    this.isInitiator = isInitiator;
    this.onConnectionStateChange = onConnectionStateChange || (() => {});
    this.onSendProgress = onSendProgress || (() => {});
    this.onReceiveProgress = onReceiveProgress || (() => {});
    this.onFileReceived = onFileReceived || (() => {});
    this.onPeerZipping = onPeerZipping || (() => {});
    this.onOffline = onOffline || (() => {});
    this.onHandshakeStateChange = onHandshakeStateChange || (() => {});

    this.isConnected = false;
    this.mode = 'ws'; // Start in 'ws' for 0ms instant connection, then auto-upgrade to 'webrtc'
    this.remotePeerId = null;
    this.pendingCandidates = [];
    this.pendingUpgrade = false;
    this.isP2pHandshaking = false;
    this.fallbackTimer = null;
    this.isClosed = false;

    // Receiving state
    this.receiveFileId = null;
    this.receiveFileName = '';
    this.receiveFileSize = 0;
    this.receiveFileType = '';
    this.receiveBytes = 0;
    this.receiveBatch = [];
    this.receiveBatchBytes = 0;
    this.blobParts = [];
    this.receiveSpeed = 0;
    this.receiveLastTime = 0;
    this.receiveLastBytes = 0;
    this.lastReceiveEmitTime = 0;

    // Resumable transfers
    this.partialTransfers = new Map();

    // Sending state
    this.activeSendingFile = null;
    this.isSending = false;
    this.abortSending = false;
    this.sendAckResolver = null;
    this.sendDoneResolver = null;
    this.sendSpeed = 0;
    this.sendLastTime = 0;
    this.sendLastBytes = 0;
    this.lastSendEmitTime = 0;

    // Heartbeat & reconnect
    this.wsReconnectTimer = null;
    this.wsReconnectDelay = 1000;
    this.heartbeatSendTimer = null;
    this.heartbeatCheckTimer = null;
    this.lastPeerHeartbeat = Date.now();

    const wsProto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const wsHost = window.location.origin === 'http://localhost:5173' ? 'localhost:3001' : window.location.host;
    this.wsProto = wsProto;
    this.wsHost = wsHost;

    this.peerConnection = null;
    this.dataChannel = null;

    this.initPeerConnection();
    this.initSocket();
  }

  initPeerConnection() {
    if (this.peerConnection) {
      try { this.peerConnection.close(); } catch { /* ignore */ }
    }
    this.peerConnection = new RTCPeerConnection({
      iceServers: ICE_SERVERS,
      iceCandidatePoolSize: 10,
      bundlePolicy: 'max-bundle'
    });
    this.dataChannel = null;
    this.pendingCandidates = [];

    this.peerConnection.onicecandidate = (event) => {
      if (event.candidate && event.candidate.candidate && event.candidate.candidate.trim() !== '') {
        console.log('[P2P] Gathered candidate:', event.candidate.type || 'unknown', event.candidate.protocol || 'udp', event.candidate.address || event.candidate.relatedAddress || '');
        const payload = event.candidate.toJSON ? event.candidate.toJSON() : event.candidate;
        this.sendWs('ice-candidate', { candidate: payload });
      }
    };

    this.peerConnection.oniceconnectionstatechange = () => {
      const state = this.peerConnection?.iceConnectionState;
      console.log('[P2P] ICE connection state:', state);
      if (state === 'connected' || state === 'completed') {
        this.setHandshaking(false);
        this.attemptUpgrade();
      } else if (state === 'failed') {
        this.setHandshaking(false);
        console.warn('[P2P] ICE checks failed, staying on WS relay');
        this.downgradeToRelay('ice_failed', true);
      }
    };

    this.peerConnection.onicegatheringstatechange = () => {
      console.log('[P2P] ICE gathering state:', this.peerConnection?.iceGatheringState);
    };

    this.peerConnection.onconnectionstatechange = () => {
      const state = this.peerConnection?.connectionState;
      console.log('[P2P] PeerConnection state:', state);
      if (state === 'connected') {
        this.setHandshaking(false);
        this.attemptUpgrade();
      } else if (state === 'failed' || state === 'closed') {
        this.setHandshaking(false);
        console.warn('[P2P] WebRTC direct path failed, staying on WS relay');
        this.downgradeToRelay('connection_failed', true);
      }
    };

    this.peerConnection.ondatachannel = (event) => {
      console.log('[P2P] Received remote DataChannel');
      this.dataChannel = event.channel;
      this.setupDataChannel();
    };
  }

  setupDataChannel() {
    if (!this.dataChannel) return;
    this.dataChannel.binaryType = 'arraybuffer';
    this.dataChannel.bufferedAmountLowThreshold = BUFFER_LOW;

    this.dataChannel.onopen = () => {
      console.log('[P2P] DataChannel open');
      this.attemptUpgrade();
    };

    this.dataChannel.onclose = () => {
      console.log('[P2P] DataChannel closed');
      this.downgradeToRelay('datachannel_closed', true);
    };

    this.dataChannel.onerror = (err) => {
      console.warn('[P2P] DataChannel error:', err);
      this.downgradeToRelay('datachannel_error', true);
    };

    this.dataChannel.onmessage = (event) => {
      this.lastPeerHeartbeat = Date.now();
      if (!this.isConnected) {
        this.setConnected('webrtc');
      }
      if (typeof event.data === 'string') {
        this.handleControlMessage(event.data);
      } else {
        this.handleBinaryChunk(event.data);
      }
    };
  }

  initSocket() {
    this.socket = new WebSocket(`${this.wsProto}//${this.wsHost}/ws`);
    this.socket.binaryType = 'arraybuffer';

    this.socket.onopen = () => {
      if (this.wsReconnectTimer) {
        clearTimeout(this.wsReconnectTimer);
        this.wsReconnectTimer = null;
      }
      this.wsReconnectDelay = 1000;
      this.onOffline(false);
      this.sendWs('join-room');
    };

    this.socket.onclose = (evt) => {
      if (this.isClosed) return;
      console.warn('[WS] Socket closed code:', evt.code);
      this.onOffline(true);
      if (this.isConnected) {
        console.log('[WS] Disconnected from peer due to socket closure');
        this.handlePeerLeft();
      }
      this.scheduleWsReconnect();
    };

    this.socket.onerror = () => {
      this.onOffline(true);
    };

    this.socket.onmessage = async (event) => {
      if (typeof event.data === 'string') {
        let msg;
        try {
          msg = JSON.parse(event.data);
        } catch {
          return;
        }

        switch (msg.type) {
          case 'user-joined':
            console.log('[P2P] Peer joined room:', msg);
            this.remotePeerId = msg.sender;
            if (msg.initiator !== undefined) {
              this.isInitiator = msg.initiator;
            }
            // 1. Instantly connect via relay with 0ms wait so file transfer is ready immediately
            this.setConnected('ws');

            // 2. Concurrently attempt direct WebRTC P2P upgrade in the background
            this.setHandshaking(true);
            this.initPeerConnection();
            if (this.isInitiator) {
              this.initiateOffer();
            }
            break;

          case 'webrtc-offer':
            console.log('[P2P] Received WebRTC offer');
            if (msg.sender) this.remotePeerId = msg.sender;
            try {
              if (this.peerConnection.signalingState !== 'stable') {
                console.warn('[P2P] PeerConnection state not stable on offer, reinitializing');
                this.initPeerConnection();
              }
              await this.peerConnection.setRemoteDescription(msg.offer);
              const answer = await this.peerConnection.createAnswer();
              await this.peerConnection.setLocalDescription(answer);
              this.sendWs('webrtc-answer', { answer });
              await this.flushQueuedCandidates();
            } catch (e) {
              console.warn('[P2P] Offer handling error:', e);
            }
            break;

          case 'webrtc-answer':
            console.log('[P2P] Received WebRTC answer');
            try {
              if (this.peerConnection.signalingState === 'have-local-offer') {
                await this.peerConnection.setRemoteDescription(msg.answer);
                await this.flushQueuedCandidates();
              }
            } catch (e) {
              console.warn('[P2P] Answer handling error:', e);
            }
            break;

          case 'ice-candidate':
            if (msg.candidate) {
              const pc = this.peerConnection;
              if (pc && pc.remoteDescription && pc.remoteDescription.type) {
                try {
                  await pc.addIceCandidate(msg.candidate);
                } catch (e) {
                  if (!e.message?.includes('ufrag')) {
                    console.warn('[P2P] Candidate error:', e);
                  }
                }
              } else {
                this.pendingCandidates.push(msg.candidate);
              }
            }
            break;

          case 'user-left':
          case 'peer-left':
            console.log('[P2P] Peer left room:', msg.sender);
            this.remotePeerId = null;
            this.handlePeerLeft();
            break;

          default:
            this.handleControlMessage(event.data);
            break;
        }
      } else {
        if (!this.isConnected) {
          this.setConnected('ws');
        }
        this.handleBinaryChunk(event.data);
      }
    };
  }

  async flushQueuedCandidates() {
    if (!this.peerConnection || !this.peerConnection.remoteDescription || this.peerConnection.signalingState === 'closed') return;
    while (this.pendingCandidates.length > 0) {
      const candidates = this.pendingCandidates.splice(0, this.pendingCandidates.length);
      await Promise.allSettled(
        candidates.map(async (cand) => {
          if (cand && cand.candidate) {
            try {
              await this.peerConnection.addIceCandidate(cand);
            } catch (e) {
              if (!e.message?.includes('ufrag')) {
                console.warn('[P2P] Queued candidate error:', e);
              }
            }
          }
        })
      );
    }
  }

  async initiateOffer() {
    if (!this.peerConnection || this.peerConnection.signalingState !== 'stable') return;
    try {
      this.dataChannel = this.peerConnection.createDataChannel('fileTransfer', { ordered: true });
      this.setupDataChannel();
      const offer = await this.peerConnection.createOffer();
      await this.peerConnection.setLocalDescription(offer);
      this.sendWs('webrtc-offer', { offer });
    } catch (e) {
      console.warn('[P2P] initiateOffer error:', e);
    }
  }

  sendWs(type, payload = {}) {
    if (this.socket && this.socket.readyState === WebSocket.OPEN) {
      this.socket.send(JSON.stringify({ type, roomId: this.roomId, ...payload }));
    }
  }

  sendControl(type, payload = {}) {
    const json = JSON.stringify({ type, ...payload });
    if (this.mode === 'webrtc' && this.dataChannel && this.dataChannel.readyState === 'open') {
      try {
        this.dataChannel.send(json);
        return;
      } catch (e) {
        console.warn('DataChannel sendControl failed, falling back to WS:', e);
      }
    }
    this.sendWs(type, payload);
  }

  handleControlMessage(rawString) {
    this.lastPeerHeartbeat = Date.now();
    let msg;
    try {
      msg = JSON.parse(rawString);
    } catch {
      return;
    }

    switch (msg.type) {
      case 'file-offer':
        this.handleFileOffer(msg);
        break;

      case 'file-ack':
        if (this.sendAckResolver) {
          this.sendAckResolver(msg.offset || 0);
          this.sendAckResolver = null;
        }
        break;

      case 'file-eof':
        this.handleFileEof();
        break;

      case 'file-done':
        if (this.sendDoneResolver) {
          this.sendDoneResolver();
          this.sendDoneResolver = null;
        }
        break;

      case 'file-cancel':
        this.handleFileCancel();
        break;

      case 'p2p-downgrade':
        console.log('[P2P] Remote peer downgraded to WS relay:', msg.reason);
        this.downgradeToRelay(msg.reason || 'peer_downgraded', false);
        break;

      case 'peer-ready':
        if (msg.mode === 'webrtc') {
          const pcState = this.peerConnection?.connectionState;
          const iceState = this.peerConnection?.iceConnectionState;
          const pcConnected = (pcState === 'connected') || (iceState === 'connected' || iceState === 'completed');
          const dcOpen = Boolean(this.dataChannel && this.dataChannel.readyState === 'open');
          if (pcConnected && dcOpen) {
            this.setConnected('webrtc');
          } else {
            console.warn('[P2P] Remote peer announced WebRTC, but local connection not ready. Staying on WS relay.');
            this.downgradeToRelay('local_webrtc_not_ready', true);
          }
        } else if (msg.mode === 'ws') {
          this.downgradeToRelay('peer_ws_mode', false);
        }
        break;

      case 'zip-start':
        this.onPeerZipping(true);
        break;

      case 'zip-done':
        this.onPeerZipping(false);
        break;

      case 'heartbeat':
        this.lastPeerHeartbeat = Date.now();
        break;
    }
  }

  downgradeToRelay(reason = 'unknown', notifyPeer = true) {
    if (this.isClosed) return;
    console.warn(`[P2P] Downgrading to WS relay (reason: ${reason})`);

    this.setHandshaking(false);
    this.pendingUpgrade = false;

    if (this.fallbackTimer) {
      clearTimeout(this.fallbackTimer);
      this.fallbackTimer = null;
    }

    if (this.dataChannel) {
      try {
        this.dataChannel.onopen = null;
        this.dataChannel.onclose = null;
        this.dataChannel.onerror = null;
        this.dataChannel.onmessage = null;
        this.dataChannel.close();
      } catch (_) {}
      this.dataChannel = null;
    }

    const modeChanged = this.mode !== 'ws';
    this.mode = 'ws';

    if (this.isConnected && modeChanged) {
      this.onConnectionStateChange('connected', 'ws');
    }

    if (notifyPeer && this.socket && this.socket.readyState === WebSocket.OPEN) {
      this.sendWs('p2p-downgrade', { reason });
    }
  }

  setHandshaking(isHandshaking) {
    if (this.isP2pHandshaking !== isHandshaking) {
      this.isP2pHandshaking = isHandshaking;
      this.onHandshakeStateChange(isHandshaking);
    }
    if (isHandshaking) {
      if (this.fallbackTimer) clearTimeout(this.fallbackTimer);
      this.fallbackTimer = setTimeout(() => {
        this.fallbackTimer = null;
        if (this.isP2pHandshaking && this.mode !== 'webrtc') {
          console.log('[P2P] Handshake timer elapsed (8s). WebRTC direct connection unavailable, remaining on WS relay.');
          this.downgradeToRelay('handshake_timeout', false);
        }
      }, 8000);
    } else {
      if (this.fallbackTimer) {
        clearTimeout(this.fallbackTimer);
        this.fallbackTimer = null;
      }
    }
  }

  attemptUpgrade() {
    if (this.isClosed) return;
    const pcState = this.peerConnection?.connectionState;
    const iceState = this.peerConnection?.iceConnectionState;
    const pcConnected = (pcState === 'connected') || (iceState === 'connected' || iceState === 'completed');
    const dcOpen = Boolean(this.dataChannel && this.dataChannel.readyState === 'open');

    if (pcConnected && dcOpen) {
      if (!this.isSending && !this.receiveFileId) {
        console.log('[P2P] WebRTC DataChannel ready and ICE verified! Upgrading to direct P2P transport');
        this.setHandshaking(false);
        this.setConnected('webrtc');
      } else {
        console.log('[P2P] Transfer in progress, queued P2P upgrade upon completion');
        this.pendingUpgrade = true;
      }
    }
  }

  setConnected(mode = 'webrtc') {
    if (mode === 'webrtc' && this.fallbackTimer) {
      clearTimeout(this.fallbackTimer);
      this.fallbackTimer = null;
    }
    const modeChanged = this.mode !== mode;
    this.mode = mode;
    if (!this.isConnected || modeChanged) {
      this.isConnected = true;
      console.log(`[P2P] Connected via: ${mode.toUpperCase()}`);
      this.onConnectionStateChange('connected', mode);
      if (mode === 'webrtc') {
        this.sendControl('peer-ready', { mode });
      }
      this.startHeartbeat();
    }
  }

  startHeartbeat() {
    this.stopHeartbeat();
    this.lastPeerHeartbeat = Date.now();
    this.heartbeatSendTimer = setInterval(() => {
      if (!this.isConnected || this.isClosed) return;
      this.sendControl('heartbeat');
    }, 2500);

    this.heartbeatCheckTimer = setInterval(() => {
      if (!this.isConnected || this.isClosed) return;
      if (typeof document !== 'undefined' && document.visibilityState === 'hidden') return;

      // If actively transferring and WebSocket is open, avoid false peer disconnects
      if (this.isSending || this.receiveFileId) {
        if (this.socket && this.socket.readyState === WebSocket.OPEN) {
          return;
        }
      }

      const silence = Date.now() - (this.lastPeerHeartbeat || 0);
      if (silence > 12000) {
        console.warn('[P2P] Peer silence timeout (12s) — treating as disconnected');
        this.stopHeartbeat();
        this.handlePeerLeft();
      }
    }, 1500);
  }

  stopHeartbeat() {
    if (this.heartbeatSendTimer) {
      clearInterval(this.heartbeatSendTimer);
      this.heartbeatSendTimer = null;
    }
    if (this.heartbeatCheckTimer) {
      clearInterval(this.heartbeatCheckTimer);
      this.heartbeatCheckTimer = null;
    }
  }

  handlePeerLeft() {
    console.log('[P2P] Peer disconnected');
    this.stopHeartbeat();
    if (this.fallbackTimer) {
      clearTimeout(this.fallbackTimer);
      this.fallbackTimer = null;
    }

    const wasSending = this.isSending;
    const interruptedFile = this.activeSendingFile;
    const sentBytes = this.sendLastBytes || 0;

    if (wasSending) {
      this.abortSending = true;
      this.isSending = false;
      this.activeSendingFile = null;
      this.onSendProgress({
        active: false,
        paused: true,
        completed: false,
        fileName: interruptedFile ? interruptedFile.name : '',
        fileSize: interruptedFile ? interruptedFile.size : 0,
        bytesTransferred: sentBytes,
        percent: (interruptedFile && interruptedFile.size > 0)
          ? Math.min(99, Math.round((sentBytes / interruptedFile.size) * 100))
          : 0,
        speed: 0,
        eta: 0
      });
    }

    if (this.receiveFileId && this.receiveBytes > 0) {
      this.onReceiveProgress({
        active: false,
        paused: true,
        completed: false,
        fileName: this.receiveFileName,
        fileSize: this.receiveFileSize,
        bytesTransferred: this.receiveBytes,
        percent: this.receiveFileSize > 0
          ? Math.min(99, Math.round((this.receiveBytes / this.receiveFileSize) * 100))
          : 0,
        speed: 0,
        eta: 0
      });
    }

    this.isConnected = false;
    this.mode = 'ws';
    this.pendingUpgrade = false;
    this.setHandshaking(false);
    if (this.dataChannel) {
      try {
        this.dataChannel.onopen = null;
        this.dataChannel.onclose = null;
        this.dataChannel.onerror = null;
        this.dataChannel.onmessage = null;
        this.dataChannel.close();
      } catch (_) {}
      this.dataChannel = null;
    }
    this.remotePeerId = null;
    this.pendingCandidates = [];

    this.initPeerConnection();
    this.onConnectionStateChange('connecting');
  }

  handleFileOffer(meta) {
    this.receiveFileId = meta.fileId;
    this.receiveFileName = meta.name;
    this.receiveFileSize = meta.size;
    this.receiveFileType = meta.fileType || 'application/octet-stream';
    this.receiveSpeed = 0;
    this.receiveLastTime = performance.now();
    this.lastReceiveEmitTime = performance.now();

    let offset = 0;
    const partial = this.partialTransfers.get(meta.fileId);
    if (partial && partial.receiveBytes < meta.size) {
      this.blobParts = partial.blobParts;
      this.receiveBatch = partial.receiveBatch;
      this.receiveBytes = partial.receiveBytes;
      this.receiveBatchBytes = partial.receiveBatchBytes || 0;
      offset = this.receiveBytes;
      console.log(`[P2P Resume] Resuming download at offset ${offset}/${meta.size}`);
    } else {
      this.blobParts = [];
      this.receiveBatch = [];
      this.receiveBatchBytes = 0;
      this.receiveBytes = 0;
    }
    this.receiveLastBytes = this.receiveBytes;

    this.sendControl('file-ack', { fileId: meta.fileId, offset });

    const percent = meta.size > 0 ? Math.min(100, Math.round((this.receiveBytes / meta.size) * 100)) : 0;
    this.onReceiveProgress({
      active: true,
      completed: false,
      fileName: this.receiveFileName,
      fileSize: this.receiveFileSize,
      bytesTransferred: this.receiveBytes,
      percent,
      speed: 0,
      eta: 0
    });
  }

  async handleBinaryChunk(data) {
    this.lastPeerHeartbeat = Date.now();

    let buffer = data;
    if (data instanceof Blob) {
      buffer = await data.arrayBuffer();
    }
    const chunkLen = buffer.byteLength || 0;
    if (chunkLen === 0) return;

    this.receiveBatch.push(buffer);
    this.receiveBatchBytes += chunkLen;
    this.receiveBytes += chunkLen;

    if (this.receiveBatchBytes >= BATCH_FLUSH_SIZE) {
      this.blobParts.push(new Blob(this.receiveBatch));
      this.receiveBatch = [];
      this.receiveBatchBytes = 0;
    }

    const now = performance.now();
    const elapsedSec = (now - this.receiveLastTime) / 1000;
    if (elapsedSec >= 0.25 || this.receiveBytes >= this.receiveFileSize) {
      const instantSpeed = (this.receiveBytes - this.receiveLastBytes) / Math.max(elapsedSec, 0.001);
      this.receiveSpeed = this.receiveSpeed === 0 ? instantSpeed : this.receiveSpeed * 0.7 + instantSpeed * 0.3;
      this.receiveLastTime = now;
      this.receiveLastBytes = this.receiveBytes;
    }

    if (now - this.lastReceiveEmitTime >= 80 || this.receiveBytes >= this.receiveFileSize) {
      this.lastReceiveEmitTime = now;
      const remainingBytes = Math.max(0, this.receiveFileSize - this.receiveBytes);
      const eta = this.receiveSpeed > 0 ? remainingBytes / this.receiveSpeed : 0;
      const percent = this.receiveFileSize > 0 ? Math.min(100, Math.round((this.receiveBytes / this.receiveFileSize) * 100)) : 100;

      this.onReceiveProgress({
        active: true,
        paused: false,
        completed: false,
        fileName: this.receiveFileName,
        fileSize: this.receiveFileSize,
        bytesTransferred: this.receiveBytes,
        percent,
        speed: this.receiveSpeed,
        eta
      });
    }

    if (this.receiveFileId) {
      this.partialTransfers.set(this.receiveFileId, {
        blobParts: this.blobParts,
        receiveBatch: this.receiveBatch,
        receiveBatchBytes: this.receiveBatchBytes,
        receiveBytes: this.receiveBytes,
        fileSize: this.receiveFileSize,
        fileName: this.receiveFileName,
        fileType: this.receiveFileType
      });
    }
  }

  handleFileEof() {
    if (this.receiveBatch.length > 0) {
      this.blobParts.push(new Blob(this.receiveBatch));
      this.receiveBatch = [];
      this.receiveBatchBytes = 0;
    }

    const finalBlob = new Blob(this.blobParts, { type: this.receiveFileType });
    const name = this.receiveFileName;
    const size = this.receiveFileSize;
    const fileId = this.receiveFileId;

    if (this.receiveFileId) {
      this.partialTransfers.delete(this.receiveFileId);
    }

    this.sendControl('file-done', { fileId });

    this.onReceiveProgress({
      active: false,
      completed: true,
      fileName: name,
      fileSize: size,
      bytesTransferred: size,
      percent: 100,
      speed: 0,
      eta: 0
    });

    this.onFileReceived(finalBlob, name);

    this.blobParts = [];
    this.receiveFileId = null;
    this.receiveFileName = '';
    this.receiveFileSize = 0;
    this.receiveBytes = 0;

    if (this.pendingUpgrade) {
      this.pendingUpgrade = false;
      this.attemptUpgrade();
    }
  }

  handleFileCancel() {
    this.blobParts = [];
    this.receiveBatch = [];
    this.receiveBatchBytes = 0;
    this.receiveBytes = 0;
    if (this.receiveFileId) {
      this.partialTransfers.delete(this.receiveFileId);
      this.receiveFileId = null;
    }
    if (this.pendingUpgrade) {
      this.pendingUpgrade = false;
      this.attemptUpgrade();
    }
    this.onReceiveProgress({
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
  }

  async sendFile(file) {
    if (!this.isConnected || this.isSending) return;
    this.isSending = true;
    this.abortSending = false;
    this.activeSendingFile = file;

    const fileId = `${file.name}-${file.size}-${file.lastModified}`;

    // Request start offset from receiver
    const ackPromise = new Promise((resolve) => {
      this.sendAckResolver = resolve;
      setTimeout(() => {
        if (this.sendAckResolver === resolve) {
          this.sendAckResolver = null;
          resolve(0);
        }
      }, 3000);
    });

    this.sendControl('file-offer', {
      fileId,
      name: file.name,
      size: file.size,
      fileType: file.type
    });

    const startOffset = await ackPromise;
    let offset = startOffset;
    if (offset > 0) {
      console.log(`[P2P Resume] Resuming send from offset ${offset}/${file.size}`);
    }

    this.sendSpeed = 0;
    this.sendLastTime = performance.now();
    this.sendLastBytes = offset;
    this.lastSendEmitTime = performance.now();

    const initialPercent = file.size > 0 ? Math.min(100, Math.round((offset / file.size) * 100)) : 0;
    this.onSendProgress({
      active: true,
      paused: false,
      completed: false,
      fileName: file.name,
      fileSize: file.size,
      bytesTransferred: offset,
      percent: initialPercent,
      speed: 0,
      eta: 0
    });

    try {
      while (offset < file.size && !this.abortSending) {
        let useDataChannel = (this.mode === 'webrtc' && this.dataChannel && this.dataChannel.readyState === 'open');

        // Backpressure management
        if (useDataChannel) {
          const dc = this.dataChannel;
          if (dc && dc.bufferedAmount > BUFFER_HIGH) {
            await new Promise((resolve) => {
              const onLow = () => {
                if (this.dataChannel) this.dataChannel.onbufferedamountlow = null;
                resolve();
              };
              if (this.dataChannel && this.dataChannel.readyState === 'open') {
                this.dataChannel.onbufferedamountlow = onLow;
              } else {
                resolve();
              }
              setTimeout(resolve, 25);
            });
          }
        } else {
          if (!this.socket || this.socket.readyState !== WebSocket.OPEN) {
            console.warn('[P2P Send] WebSocket is closed, aborting send');
            this.abortSending = true;
            break;
          }
          if (this.socket.bufferedAmount > BUFFER_HIGH) {
            await new Promise((resolve) => setTimeout(resolve, 5));
          }
        }

        if (this.abortSending) break;

        const blockEnd = Math.min(file.size, offset + BLOCK_SIZE);
        const blockSlice = file.slice(offset, blockEnd);
        const blockBuffer = await blockSlice.arrayBuffer();

        let blockPos = 0;
        while (blockPos < blockBuffer.byteLength && !this.abortSending) {
          useDataChannel = (this.mode === 'webrtc' && this.dataChannel && this.dataChannel.readyState === 'open');

          if (useDataChannel) {
            const dc = this.dataChannel;
            if (dc && dc.bufferedAmount > BUFFER_HIGH) {
              await new Promise((resolve) => {
                const onLow = () => {
                  if (this.dataChannel) this.dataChannel.onbufferedamountlow = null;
                  resolve();
                };
                if (this.dataChannel && this.dataChannel.readyState === 'open') {
                  this.dataChannel.onbufferedamountlow = onLow;
                } else {
                  resolve();
                }
                setTimeout(resolve, 25);
              });
            }
          } else {
            if (!this.socket || this.socket.readyState !== WebSocket.OPEN) {
              console.warn('[P2P Send] WebSocket is closed, aborting send');
              this.abortSending = true;
              break;
            }
            if (this.socket.bufferedAmount > BUFFER_HIGH) {
              await new Promise((resolve) => setTimeout(resolve, 5));
            }
          }

          if (this.abortSending) break;

          // Re-evaluate after possible backpressure delay
          useDataChannel = (this.mode === 'webrtc' && this.dataChannel && this.dataChannel.readyState === 'open');

          const chunkEnd = Math.min(blockBuffer.byteLength, blockPos + CHUNK_SIZE);
          const chunkLen = chunkEnd - blockPos;
          const chunk = new Uint8Array(blockBuffer, blockPos, chunkLen);

          if (useDataChannel) {
            try {
              this.dataChannel.send(chunk);
            } catch (err) {
              console.warn('[P2P Send] DataChannel send failed, downgrading to WS relay:', err);
              this.downgradeToRelay('send_error', true);
              if (this.socket && this.socket.readyState === WebSocket.OPEN) {
                try {
                  this.socket.send(chunk);
                } catch (wsErr) {
                  console.error('[P2P Send] WS fallback failed:', wsErr);
                  this.abortSending = true;
                  break;
                }
              } else {
                this.abortSending = true;
                break;
              }
            }
          } else {
            try {
              this.socket.send(chunk);
            } catch (err) {
              console.warn('[P2P Send] WS send failed:', err);
              this.abortSending = true;
              break;
            }
          }

          blockPos += chunkLen;
          offset += chunkLen;

          const now = performance.now();
          const elapsedSec = (now - this.sendLastTime) / 1000;
          if (elapsedSec >= 0.25 || offset >= file.size) {
            const instantSpeed = (offset - this.sendLastBytes) / Math.max(elapsedSec, 0.001);
            this.sendSpeed = this.sendSpeed === 0 ? instantSpeed : this.sendSpeed * 0.7 + instantSpeed * 0.3;
            this.sendLastTime = now;
            this.sendLastBytes = offset;
          }

          if (now - this.lastSendEmitTime >= 80 || offset >= file.size) {
            this.lastSendEmitTime = now;
            const remainingBytes = Math.max(0, file.size - offset);
            const eta = this.sendSpeed > 0 ? remainingBytes / this.sendSpeed : 0;
            const rawPercent = file.size > 0 ? Math.min(100, Math.round((offset / file.size) * 100)) : 100;
            const percent = rawPercent === 100 ? 99 : rawPercent;

            this.onSendProgress({
              active: true,
              paused: false,
              completed: false,
              fileName: file.name,
              fileSize: file.size,
              bytesTransferred: offset,
              percent,
              speed: this.sendSpeed,
              eta
            });
          }
        }
      }

      if (!this.abortSending) {
        // Drain local network buffer
        let useDataChannel = (this.mode === 'webrtc' && this.dataChannel && this.dataChannel.readyState === 'open');
        if (useDataChannel) {
          while (this.dataChannel && this.dataChannel.readyState === 'open' && this.dataChannel.bufferedAmount > 0 && !this.abortSending) {
            await new Promise((resolve) => setTimeout(resolve, 20));
          }
        } else {
          while (this.socket && this.socket.readyState === WebSocket.OPEN && this.socket.bufferedAmount > 0 && !this.abortSending) {
            await new Promise((resolve) => setTimeout(resolve, 15));
          }
        }

        const donePromise = new Promise((resolve) => {
          this.sendDoneResolver = resolve;
          setTimeout(() => {
            if (this.sendDoneResolver === resolve) {
              this.sendDoneResolver = null;
              resolve();
            }
          }, 6000);
        });

        this.sendControl('file-eof', { fileId });
        await donePromise;

        this.onSendProgress({
          active: false,
          paused: false,
          completed: true,
          fileName: file.name,
          fileSize: file.size,
          bytesTransferred: file.size,
          percent: 100,
          speed: 0,
          eta: 0
        });
      }
    } catch (err) {
      console.error('[P2P Send Error]', err);
    } finally {
      this.isSending = false;
      this.activeSendingFile = null;
      if (this.pendingUpgrade) {
        this.pendingUpgrade = false;
        this.attemptUpgrade();
      }
    }
  }

  cancelSend() {
    this.abortSending = true;
    this.isSending = false;
    this.activeSendingFile = null;
    if (this.pendingUpgrade) {
      this.pendingUpgrade = false;
      this.attemptUpgrade();
    }
    this.sendControl('file-cancel', {});
    this.onSendProgress({
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
  }

  reconnect() {
    if (this.isClosed) return;
    if (this.socket && (this.socket.readyState === WebSocket.OPEN || this.socket.readyState === WebSocket.CONNECTING)) {
      return;
    }
    if (this.wsReconnectTimer) {
      clearTimeout(this.wsReconnectTimer);
      this.wsReconnectTimer = null;
    }
    this.wsReconnectDelay = 1000;
    console.log('[WS] Instant reconnect triggered');
    try {
      this.initSocket();
    } catch (e) {
      console.warn('[WS] Reconnect failed:', e);
      this.scheduleWsReconnect();
    }
  }

  scheduleWsReconnect() {
    if (this.isClosed) return;
    if (this.wsReconnectTimer) return;
    const delay = this.wsReconnectDelay || 1000;
    this.wsReconnectDelay = Math.min((this.wsReconnectDelay || 1000) * 2, 8000);
    console.log(`[WS] Reconnecting in ${delay}ms…`);
    this.wsReconnectTimer = setTimeout(() => {
      this.wsReconnectTimer = null;
      if (this.isClosed) return;
      try {
        this.initSocket();
      } catch (e) {
        console.warn('[WS] Reconnect failed:', e);
        this.scheduleWsReconnect();
      }
    }, delay);
  }

  close() {
    this.isClosed = true;
    this.setHandshaking(false);
    this.stopHeartbeat();
    if (this.wsReconnectTimer) {
      clearTimeout(this.wsReconnectTimer);
      this.wsReconnectTimer = null;
    }
    if (this.fallbackTimer) clearTimeout(this.fallbackTimer);
    try {
      this.sendControl('peer-left', {});
      this.sendWs('leave-room');
    } catch { /* ignore */ }
    if (this.dataChannel) {
      try { this.dataChannel.close(); } catch { /* ignore */ }
    }
    if (this.peerConnection) {
      try { this.peerConnection.close(); } catch { /* ignore */ }
    }
    if (this.socket) {
      try { this.socket.close(); } catch { /* ignore */ }
    }
    this.isConnected = false;
    this.isSending = false;
  }
}
