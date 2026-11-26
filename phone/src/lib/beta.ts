// The phone app betas, on the account host: GET /beta says where the account
// stands, POST /beta/<platform> signs it up. The operator admits each sign-up
// by hand in the admin dashboard and adds its email in TestFlight or Play
// Console, so a sign-up always carries the email the invite goes to.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

export type Platform = "ios" | "android";
export const PLATFORMS: Platform[] = ["ios", "android"];

export interface Signup {
  status: "none" | "pending" | "admitted" | "declined";
  email: string | null;
  retry_at: number | null;
}

export interface Signups {
  /** The account's GitHub email, to start the field with. */
  email: string | null;
  ios: Signup;
  android: Signup;
}

export const HIDE_BETA = "arakun.hideBeta";
/** The account flag, granted in the admin dashboard, that shows the sign-up. The desktop app reads the same one. */
export const BETA_FLAG = "mobile_beta";

export const STORE: Record<Platform, string> = { ios: "TestFlight", android: "Google Play" };
export const NAME: Record<Platform, string> = { ios: "iOS", android: "Android" };
/** Whose address the store invites. */
export const ACCOUNT: Record<Platform, string> = { ios: "Apple ID", android: "Google account" };

/** The phone this page runs on, when it is one. iPads say they are Macs, with a touchscreen. */
export function thisPlatform(ua = navigator.userAgent, touch = navigator.maxTouchPoints): Platform | null {
  if (/iPhone|iPad|iPod/.test(ua) || (/Macintosh/.test(ua) && touch > 1)) return "ios";
  if (/Android/.test(ua)) return "android";
  return null;
}

/** Whether the account may sign up for it now: never did, or was declined long enough ago. */
export function canJoin(s: Signup, now = Date.now() / 1000): boolean {
  if (s.status === "none") return true;
  return s.status === "declined" && (s.retry_at == null || s.retry_at <= now);
}

/** What one platform's row says. */
export function betaLine(p: Platform, s: Signup, now = Date.now() / 1000): string {
  switch (s.status) {
    case "pending":
      return `Signed up. We add testers by hand; the ${STORE[p]} invite will go to ${s.email}.`;
    case "admitted":
      return `You are in. Look for the ${STORE[p]} invite at ${s.email}.`;
    case "declined":
      if (s.retry_at && s.retry_at > now) {
        const days = Math.ceil((s.retry_at - now) / 86_400);
        return `Not this time. You can sign up again in ${days === 1 ? "1 day" : `${days} days`}.`;
      }
      return "Not last time, but you can sign up again.";
    default:
      return `Get Arakun for ${NAME[p]} through ${STORE[p]}.`;
  }
}

async function readSignups(): Promise<Signups | null> {
  const res = await fetch("/beta", { cache: "no-store", credentials: "same-origin" });
  // Signed out: nothing to show; the sign-in screen takes over elsewhere.
  if (res.status === 401) return null;
  if (!res.ok) throw new Error(String(res.status));
  return (await res.json()) as Signups;
}

/** Where the account stands with the betas. Only on the account host: a computer's own page has no account. */
export function useBeta(enabled: boolean) {
  return useQuery({ queryKey: ["beta"], queryFn: readSignups, enabled, staleTime: 60_000, retry: false });
}

/** The service's reason when it gives one, as the desktop app shows it. */
async function why(res: Response): Promise<string> {
  try {
    const body = (await res.json()) as { error?: string };
    if (body.error) return body.error;
  } catch {
    // No body to read.
  }
  return "Could not sign up. Try again.";
}

/** Signs up for one beta with the email the invite goes to. */
export function useJoinBeta() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async ({ platform, email }: { platform: Platform; email: string }): Promise<Signup> => {
      const res = await fetch(`/beta/${platform}`, {
        method: "POST",
        cache: "no-store",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(email ? { email } : {}),
      });
      if (!res.ok) throw new Error(await why(res));
      return (await res.json()) as Signup;
    },
    onSuccess: (signup, { platform }) => {
      qc.setQueryData<Signups | null>(["beta"], (b) => (b ? { ...b, [platform]: signup } : b));
    },
  });
}
