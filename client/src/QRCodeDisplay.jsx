import { QRCodeSVG } from 'qrcode.react';

export default function QRCodeDisplay({ value, size = 140 }) {
  return <QRCodeSVG value={value} size={size} level="M" />;
}
