import { encode } from "uqr";

/// A QR code as inline SVG: the page policy allows no image from a data: URL,
/// and the key must not leave the page for a QR service. Always dark on light
/// with a quiet zone, whatever the color scheme, so every app can read it.
export function QrCode({ text, label }: { text: string; label: string }) {
  const { data, size } = encode(text, { ecc: "M", border: 4 });
  let path = "";
  data.forEach((row, y) =>
    row.forEach((dark, x) => {
      if (dark) path += `M${x} ${y}h1v1h-1z`;
    }),
  );
  return (
    <svg className="qr" viewBox={`0 0 ${size} ${size}`} role="img" aria-label={label} shapeRendering="crispEdges">
      <rect className="qr-light" width={size} height={size} />
      <path className="qr-dark" d={path} />
    </svg>
  );
}
