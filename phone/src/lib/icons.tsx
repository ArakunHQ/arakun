import { ICON_DATA } from "./icon-data";

export type IconName = keyof typeof ICON_DATA;
export type IconSize = 12 | 14 | 16 | 20;

/**
 * An icon from assets/icons. The markup comes from our own generated table,
 * never from user text, so it is safe to inline.
 */
export function Icon({ name, size = 16 }: { name: IconName; size?: IconSize }) {
  const icon: { body: string; filled?: boolean } = ICON_DATA[name];
  return (
    <svg
      viewBox="0 0 24 24"
      width={size}
      height={size}
      fill={icon.filled ? "currentColor" : "none"}
      stroke={icon.filled ? "none" : "currentColor"}
      strokeWidth={1.75}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      dangerouslySetInnerHTML={{ __html: icon.body }}
    />
  );
}
