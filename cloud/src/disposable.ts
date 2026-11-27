// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// Mail domains that hand out throwaway inboxes. An address there gets no
// sign-in code: an account made with one would be gone with the inbox, and
// such accounts are how free cloud would be farmed. The operator adds more
// without a deploy, as a JSON list of domains in the CONFIG key
// `blocked_email_domains`.

/// The built-in list: the services seen most, not every one there is.
const BUILT_IN = new Set([
  "10minutemail.com",
  "10minutemail.net",
  "20minutemail.com",
  "33mail.com",
  "anonaddy.me",
  "burnermail.io",
  "byom.de",
  "discard.email",
  "dispostable.com",
  "dropmail.me",
  "emailondeck.com",
  "emltmp.com",
  "fakeinbox.com",
  "fakemail.net",
  "getairmail.com",
  "getnada.com",
  "guerrillamail.biz",
  "guerrillamail.com",
  "guerrillamail.de",
  "guerrillamail.info",
  "guerrillamail.net",
  "guerrillamail.org",
  "guerrillamailblock.com",
  "harakirimail.com",
  "inboxbear.com",
  "inboxkitten.com",
  "incognitomail.org",
  "jetable.org",
  "mailcatch.com",
  "maildrop.cc",
  "mailinator.com",
  "mailinator.net",
  "mailinator2.com",
  "mailnesia.com",
  "mailpoof.com",
  "mailsac.com",
  "mailtemp.net",
  "mintemail.com",
  "moakt.com",
  "mohmal.com",
  "mytemp.email",
  "mytrashmail.com",
  "nada.email",
  "sharklasers.com",
  "spam4.me",
  "spambox.us",
  "spamgourmet.com",
  "temp-mail.io",
  "temp-mail.org",
  "tempail.com",
  "tempinbox.com",
  "tempmail.com",
  "tempmail.dev",
  "tempmail.net",
  "tempmail.plus",
  "tempmailo.com",
  "tempr.email",
  "throwawaymail.com",
  "tmail.ws",
  "tmpmail.net",
  "tmpmail.org",
  "trash-mail.com",
  "trashmail.com",
  "trashmail.de",
  "trashmail.net",
  "yopmail.com",
  "yopmail.fr",
  "yopmail.net",
]);

const MAX_LIST = 10_000;

/// Whether the domain, or a domain it is under, gives out throwaway inboxes:
/// many of these services take any name under their domain too.
export async function disposable(domain: string, config: KVNamespace): Promise<boolean> {
  const extra = await operatorList(config);
  const labels = domain.split(".");
  for (let i = 0; i < labels.length - 1; i++) {
    const d = labels.slice(i).join(".");
    if (BUILT_IN.has(d) || extra.has(d)) return true;
  }
  return false;
}

/// The operator's domains. A list that is missing or not a list of strings
/// counts as empty, so a typo there does not stop every sign-in.
async function operatorList(config: KVNamespace): Promise<Set<string>> {
  let value: unknown;
  try {
    value = await config.get("blocked_email_domains", "json");
  } catch {
    console.error("the CONFIG key blocked_email_domains is not JSON");
    return new Set();
  }
  if (!Array.isArray(value)) return new Set();
  return new Set(
    value
      .slice(0, MAX_LIST)
      .filter((d): d is string => typeof d === "string")
      .map((d) => d.trim().toLowerCase()),
  );
}
