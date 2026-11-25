import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { LAST_MACHINE, useMachines, useMarkNoticeRead, useRemoveMachine, useSignedInSessions, useSignOut } from "./account";
import { lastToast } from "./toast";

let qc: QueryClient;
const wrapper = ({ children }: { children: ReactNode }) => createElement(QueryClientProvider, { client: qc }, children);
const pc = { label: "aaaaaaaaaaaaaaaa", pub: "AAAA", name: "pc-a" };

function fakeRelay(over: Record<string, unknown> = {}) {
  (globalThis as any).RELAY = {
    forgetAccountState: vi.fn(),
    loggedOut: (s: number) => s === 204 || s === 401,
    signOutFailed: () => "Signing out did not reach h.",
    ...over,
  };
}

beforeEach(() => {
  localStorage.clear();
  qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  delete (globalThis as any).RELAY;
});

describe("the account hooks", () => {
  it("lists the computers under the key machines", async () => {
    fakeRelay({ machines: async () => ({ list: [pc] }) });
    const { result } = renderHook(() => useMachines(), { wrapper });
    await waitFor(() => expect(result.current.data).toEqual({ list: [pc] }));
    expect(qc.getQueryData(["machines"])).toEqual({ list: [pc] });
  });

  it("signs out when the account host says the session is gone", async () => {
    fakeRelay({ machines: async () => ({ signedOut: true }) });
    qc.setQueryData(["me"], { screen: "list", me: { login: "rik" }, machines: [pc] });
    localStorage.setItem(LAST_MACHINE, pc.label);
    renderHook(() => useMachines(), { wrapper });
    await waitFor(() => expect(qc.getQueryData(["me"])).toEqual({ screen: "signin" }));
    expect((globalThis as any).RELAY.forgetAccountState).toHaveBeenCalledWith(localStorage);
  });

  it("forgets the computer opened last once it is removed", async () => {
    const forgetMachine = vi.fn(async () => ({ ok: true }));
    fakeRelay({ forgetMachine, machines: async () => ({ list: [] }) });
    localStorage.setItem(LAST_MACHINE, pc.label);
    const { result } = renderHook(() => useRemoveMachine(), { wrapper });
    await act(() => result.current.mutateAsync(pc));
    expect(forgetMachine).toHaveBeenCalledWith(pc.label);
    expect(localStorage.getItem(LAST_MACHINE)).toBeNull();
  });

  it("says so when a computer could not be removed", async () => {
    fakeRelay({ forgetMachine: async () => ({ ok: false, status: 500 }), machines: async () => ({ list: [pc] }) });
    localStorage.setItem(LAST_MACHINE, pc.label);
    const { result } = renderHook(() => useRemoveMachine(), { wrapper });
    await act(() => result.current.mutateAsync(pc));
    expect(lastToast()).toBe("Could not remove pc-a");
    expect(localStorage.getItem(LAST_MACHINE)).toBe(pc.label);
  });

  it("drops a notice at once and tells the account host it was read", async () => {
    fakeRelay();
    const fetch = vi.fn(async () => new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetch);
    qc.setQueryData(["me"], { screen: "list", me: { login: "rik", notices: [{ id: 7, text: "hi" }, { id: 8, text: "yo" }] } });
    const { result } = renderHook(() => useMarkNoticeRead(), { wrapper });
    await act(() => result.current.mutateAsync(7));
    expect((qc.getQueryData(["me"]) as any).me.notices).toEqual([{ id: 8, text: "yo" }]);
    expect(fetch).toHaveBeenCalledWith("/notices/7/read", { method: "POST", credentials: "same-origin" });
  });

  it("lists the signed-in sessions under the key sessions", async () => {
    fakeRelay({ sessions: async () => ({ list: [{ id: "x" }] }) });
    const { result } = renderHook(() => useSignedInSessions(), { wrapper });
    await waitFor(() => expect(result.current.data).toEqual({ list: [{ id: "x" }] }));
  });

  it("shows sign-in only once the account host ended the session", async () => {
    fakeRelay();
    vi.stubGlobal("fetch", vi.fn(async () => new Response(null, { status: 204 })));
    qc.setQueryData(["me"], { screen: "list", me: { login: "rik" } });
    const { result } = renderHook(() => useSignOut(), { wrapper });
    await act(() => result.current.mutateAsync());
    expect(qc.getQueryData(["me"])).toEqual({ screen: "signin" });
  });

  it("stays signed in when signing out did not reach the account host", async () => {
    fakeRelay();
    const alert = vi.fn();
    vi.stubGlobal("alert", alert);
    vi.stubGlobal("fetch", vi.fn(async () => new Response(null, { status: 502 })));
    qc.setQueryData(["me"], { screen: "list", me: { login: "rik" } });
    const { result } = renderHook(() => useSignOut(), { wrapper });
    await act(() => result.current.mutateAsync());
    expect(alert).toHaveBeenCalledWith("Signing out did not reach h.");
    expect(qc.getQueryData(["me"])).toEqual({ screen: "list", me: { login: "rik" } });
  });
});
