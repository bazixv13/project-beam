import { useEffect, useState } from 'react';
import { Html5Qrcode } from 'html5-qrcode';
import { XCircle } from 'lucide-react';

export default function QRScanner({ onScan, onClose, onPermissionDenied }) {
  const [errorMsg, setErrorMsg] = useState('');

  useEffect(() => {
    let html5QrCode;
    let isMounted = true;

    const startScanner = async () => {
      try {
        html5QrCode = new Html5Qrcode("reader");
        await html5QrCode.start(
          { facingMode: "environment" },
          { fps: 10, qrbox: { width: 220, height: 220 } },
          (decodedText) => {
            if (isMounted) {
              onScan(decodedText);
            }
          },
          (err) => {
            // Ignore normal scanning errors (e.g., QR not found yet)
          }
        );
      } catch (err) {
        console.error("Camera access error:", err);
        if (isMounted) {
          setErrorMsg("Camera access denied or no camera found.");
          setTimeout(() => {
            onPermissionDenied();
            onClose();
          }, 2000);
        }
      }
    };
    
    startScanner();

    return () => {
      isMounted = false;
      if (html5QrCode && html5QrCode.isScanning) {
        html5QrCode.stop().then(() => {
          html5QrCode.clear();
        }).catch(console.error);
      }
    };
  }, [onScan, onClose, onPermissionDenied]);

  return (
    <div className="qr-scanner-overlay">
      <div className="qr-scanner-container">
        <button className="qr-close-btn" onClick={onClose}><XCircle size={28} /></button>
        {errorMsg ? (
          <div className="qr-error">{errorMsg}</div>
        ) : (
          <div id="reader" className="qr-reader-box"></div>
        )}
      </div>
    </div>
  );
}
