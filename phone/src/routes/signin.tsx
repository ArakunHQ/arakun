import { useSaid } from "../lib/account";
import { Icon } from "../lib/icons";
import { toast } from "../lib/toast";
import "./signin.css";

const INSTALL = "curl -fsSL https://arakun.dev/install | sh";

function copyInstall() {
  // The clipboard needs a secure origin; the sign-in page is on https.
  navigator.clipboard?.writeText(INSTALL).then(
    () => toast("Copied"),
    () => toast("Could not copy. Select the line instead."),
  );
}

export function SignIn() {
  const said = useSaid();
  return (
    <div className="screen signin">
      <div className="sitop">Arakun</div>
      <main className="simain">
        <img className="simascot" src="mascot.svg" alt="" />
        <div className="sicopy">
          <div className="sieyebrow">app.arakun.dev</div>
          <h1>Reach your coding agents from any screen.</h1>
          <p>
            Sign in with the GitHub account you use in Arakun on your computer. Your sessions stay on that computer. This page talks to it end-to-end
            encrypted.
          </p>
          <a className="btn" href="/auth/github/start">
            <Icon name="github" size={20} />
            Sign in with GitHub
          </a>
          {said ? <div className="sierr">{said}</div> : null}
          <div className="siinstall">
            <div className="silabel">
              <Icon name="monitor" />
              No Arakun yet? On your computer, run
            </div>
            <div className="siline">
              <code>
                <span className="siprompt">$</span> curl -fsSL <span className="siurl">https://arakun.dev/install</span> | sh
              </code>
              <button type="button" className="sicopybtn" aria-label="Copy the install command" onClick={copyInstall}>
                <Icon name="copy" />
              </button>
            </div>
          </div>
        </div>
      </main>
      <div className="sifoot">
        <span>End-to-end encrypted</span>
        <span>Works on phone, tablet and desktop</span>
      </div>
    </div>
  );
}
