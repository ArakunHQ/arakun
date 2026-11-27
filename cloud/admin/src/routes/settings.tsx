// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { AuthenticatorSetup } from "../components/AuthenticatorSetup";

export const Route = createFileRoute("/settings")({
  component: Settings,
});

function Settings() {
  const [state, setState] = useState<"idle" | "replacing" | "done">("idle");
  return (
    <>
      <h1>Settings</h1>
      <section className="section">
        <h2>Authenticator app</h2>
        <p className="muted">
          Every sign-in asks for a code from your authenticator app after GitHub. Move it to a new phone or app here; you need a code from
          the current one.
        </p>
        {state === "done" && (
          <p className="message" data-tone="success" role="status">
            The new authenticator app is set up. The old one no longer works.
          </p>
        )}
        {state === "replacing" ? (
          <>
            <AuthenticatorSetup replacing onDone={() => setState("done")} />
            <div className="form-actions">
              <button type="button" className="button quiet" onClick={() => setState("idle")}>
                Cancel
              </button>
            </div>
          </>
        ) : (
          <div className="form-actions">
            <button type="button" className="button" onClick={() => setState("replacing")}>
              Replace authenticator app
            </button>
          </div>
        )}
      </section>
    </>
  );
}
