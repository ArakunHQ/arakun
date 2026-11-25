// The page scripts (relay.js, e2e.js, diagrams.js) still load as plain
// scripts and set globals. These types cover what the app calls, no more.

export interface Machine {
  label: string;
  name: string;
  pub: string;
  [k: string]: unknown;
}

export type MachinesResult = { list: Machine[] } | { signedOut: true } | { blocked: true; reason: string | null };
export type ActResult = { ok: boolean; status?: number } | { signedOut: true };

export interface AccountMe {
  [k: string]: unknown;
}

export type BootResult = { screen: string; [k: string]: unknown };

export type TapRoute = { go: "stay" | "open" | "list" } | { go: "pick"; machine: Machine };

export interface RelayHandlers {
  onopen: (info: object) => void;
  onmessage: (msg: any) => void;
  [k: string]: ((...args: any[]) => void) | undefined;
}

export interface RelayLink {
  send(obj: unknown): void;
  fetch(path: string, init?: { method?: string; body?: unknown }): Promise<Response>;
  close(): void;
  readonly ready: boolean;
}

export interface RelayApi {
  detect(): Promise<unknown>;
  boot(): Promise<BootResult>;
  machines(): Promise<MachinesResult>;
  forgetMachine(label: string): Promise<ActResult>;
  removeQuestion(name: string): string;
  sessions(): Promise<{ list: unknown[] } | { signedOut: true }>;
  signOutSession(id: string): Promise<ActResult>;
  signedInRows(list: unknown[], esc: (s: unknown) => string, ago: (unix: number) => string): string;
  accountHost(): string;
  unreachable(): string;
  loggedOut(status: number): boolean;
  signOutFailed(): string;
  tapRoute(screen: string, account: unknown, picked: boolean, last: string | null): TapRoute;
  tapSentence(): string;
  sentence(reason: string, message?: string): string;
  forgetOldPairing(): void;
  noticeLink(url: string): string | null;
  pushNote(machines: Machine[], current: string): string;
  noticeElsewhere(): string;
  noticeOutcome(pending: string, msg: { type?: string; id?: string } | null): "elsewhere" | "opened" | "unrelated";
  scoped(key: string, label: string | null): string;
  forgetAccountState(storage: Storage): void;
  open(machine: Pick<Machine, "label" | "pub">, handlers: RelayHandlers): RelayLink;
}

export interface E2eApi {
  b64url(bytes: Uint8Array): string;
  fromB64url(s: string): Uint8Array | null;
  utf8(s: string): Uint8Array;
  text(bytes: Uint8Array): string;
}

export interface DiagramsApi {
  /** The HTML card for a fenced block, or null when it is not a drawing. */
  render(lang: string, text: string): string | null;
}

// Read at use, not at import: the scripts load before the bundle in the page,
// and a test can set a fake on globalThis first.
function lazy<T extends object>(name: string): T {
  return new Proxy({} as T, {
    get(_, key) {
      const api = (globalThis as Record<string, unknown>)[name] as T | undefined;
      if (!api) throw new Error(`${name} is not loaded`);
      return api[key as keyof T];
    },
  });
}

export const RELAY = lazy<RelayApi>("RELAY");
export const E2E = lazy<E2eApi>("E2E");
export const DIAGRAMS = lazy<DiagramsApi>("Diagrams");
