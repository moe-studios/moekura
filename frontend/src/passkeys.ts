// Passkeys (WebAuthn): adding one on the account page, and logging in with
// one, on the login page (among the name field's suggestions, or with the
// button) or instead of a two-factor code. The server makes the options
// and checks the answers; this passes them between it and the browser,
// with the binary fields as base64url text.

import { t } from "./i18n.ts";

type Json = Record<string, unknown>;

/** Bytes as unpadded base64url, as the server sends and expects them. */
export function toBase64url(bytes: ArrayBuffer | Uint8Array): string {
  const array = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  let binary = "";
  for (const byte of array) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

export function fromBase64url(text: string): Uint8Array<ArrayBuffer> {
  const base64 = text.replaceAll("-", "+").replaceAll("_", "/");
  const padded = base64 + "=".repeat((4 - (base64.length % 4)) % 4);
  return Uint8Array.from(atob(padded), (c) => c.charCodeAt(0));
}

function descriptors(list: unknown): PublicKeyCredentialDescriptor[] | undefined {
  if (!Array.isArray(list)) return undefined;
  return list.map((d: Json) => ({ ...d, id: fromBase64url(d["id"] as string) }) as unknown as PublicKeyCredentialDescriptor);
}

/** The server's registration options (`publicKey`), as `credentials.create` takes them. */
export function creationOptions(json: Json): PublicKeyCredentialCreationOptions {
  const user = json["user"] as Json;
  const options: Json = {
    ...json,
    challenge: fromBase64url(json["challenge"] as string),
    user: { ...user, id: fromBase64url(user["id"] as string) },
  };
  const exclude = descriptors(json["excludeCredentials"]);
  if (exclude) options["excludeCredentials"] = exclude;
  return options as unknown as PublicKeyCredentialCreationOptions;
}

/** The server's login options (`publicKey`), as `credentials.get` takes them. */
export function requestOptions(json: Json): PublicKeyCredentialRequestOptions {
  const options: Json = { ...json, challenge: fromBase64url(json["challenge"] as string) };
  const allow = descriptors(json["allowCredentials"]);
  if (allow) options["allowCredentials"] = allow;
  return options as unknown as PublicKeyCredentialRequestOptions;
}

function attestation(response: AuthenticatorAttestationResponse): Json {
  return {
    clientDataJSON: toBase64url(response.clientDataJSON),
    attestationObject: toBase64url(response.attestationObject),
    transports: response.getTransports?.() ?? [],
  };
}

function assertion(response: AuthenticatorAssertionResponse): Json {
  return {
    clientDataJSON: toBase64url(response.clientDataJSON),
    authenticatorData: toBase64url(response.authenticatorData),
    signature: toBase64url(response.signature),
    userHandle: response.userHandle ? toBase64url(response.userHandle) : null,
  };
}

/** A new passkey or a login's answer, as the server reads it. */
export function credentialJSON(credential: PublicKeyCredential): Json {
  const response = credential.response;
  const answer =
    "attestationObject" in response
      ? attestation(response as AuthenticatorAttestationResponse)
      : assertion(response as AuthenticatorAssertionResponse);
  return {
    id: credential.id,
    rawId: toBase64url(credential.rawId),
    type: credential.type,
    response: answer,
    clientExtensionResults: credential.getClientExtensionResults(),
  };
}

/** The server said no; the message is for the person. */
export class Refusal extends Error {
  override name = "Refusal";
}

/** What to tell the person about `error`, if anything. */
export function problem(error: unknown): string | null {
  if (!(error instanceof Error)) return t("passkey-failed", "Something went wrong. Please try again.");
  switch (error.name) {
    // Aborted on purpose: the button took over from the suggestions.
    case "AbortError":
      return null;
    // Cancelled, timed out, or no passkey for the site on the device.
    case "NotAllowedError":
      return t("passkey-cancelled", "No passkey was used. Try again when you're ready.");
    // Making one the device already has for the account.
    case "InvalidStateError":
      return t("passkey-exists", "This device already has a passkey for your account.");
    case "Refusal":
      return error.message;
    default:
      return t("passkey-failed", "Something went wrong. Please try again.");
  }
}

interface Reply {
  token?: string;
  options?: { publicKey: Json };
  redirect?: string;
  error?: string;
}

async function post(url: string, body: unknown = {}): Promise<Reply> {
  const response = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    credentials: "same-origin",
    body: JSON.stringify(body),
  });
  let reply: Reply = {};
  try {
    reply = (await response.json()) as Reply;
  } catch {
    // An error page, not JSON.
  }
  if (!response.ok && !reply.redirect) {
    if (reply.error) throw new Refusal(reply.error);
    if (response.status === 429) throw new Refusal(t("passkey-too-many", "Too many tries. Wait a minute, then try again."));
    throw new Refusal(t("error-status", "Error {$status}", { status: response.status }));
  }
  return reply;
}

/** Follows the server's `redirect`; true if there was one. */
function follow(reply: Reply): boolean {
  if (!reply.redirect) return false;
  const to = new URL(reply.redirect, location.href);
  if (to.pathname === location.pathname && to.search === location.search) {
    // Only the fragment differs, which alone wouldn't load the page again.
    location.hash = to.hash;
    location.reload();
  } else {
    location.assign(to);
  }
  return true;
}

function show(box: HTMLElement | null, message: string | null): void {
  if (!box) return;
  box.textContent = message ?? "";
  box.hidden = message === null;
}

function supported(): boolean {
  return typeof PublicKeyCredential !== "undefined" && !!navigator.credentials;
}

function enableRegistration(form: HTMLFormElement, root: Document): void {
  const error = form.querySelector<HTMLElement>("[data-passkey-error]");
  const button = form.querySelector<HTMLButtonElement>("button[type=submit]");
  form.hidden = false;
  for (const note of root.querySelectorAll<HTMLElement>("[data-passkey-unsupported]")) note.hidden = true;
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    show(error, null);
    if (button) button.disabled = true;
    const data = new FormData(form);
    (async () => {
      const asked = await post("/settings/passkeys/options", {
        name: data.get("name") ?? "",
        password: data.get("password") ?? "",
      });
      if (follow(asked) || !asked.options) return;
      const credential = await navigator.credentials.create({ publicKey: creationOptions(asked.options.publicKey) });
      if (!credential) return;
      const added = await post("/settings/passkeys", {
        token: asked.token,
        credential: credentialJSON(credential as PublicKeyCredential),
      });
      follow(added);
    })()
      .catch((e: unknown) => show(error, problem(e)))
      .finally(() => {
        if (button) button.disabled = false;
      });
  });
}

/** A passkey the browser gave for a login, and the login it's for. */
interface Picked {
  token: string | undefined;
  credential: Credential;
}

function enableLogin(box: HTMLElement): void {
  const error = box.querySelector<HTMLElement>("[data-passkey-error]");
  const button = box.querySelector<HTMLButtonElement>("button");
  const next = box.dataset["next"] || null;
  let waiting: AbortController | null = null;
  box.hidden = false;

  /** Asks the browser for a passkey; null if none was picked. */
  async function pick(conditional: boolean): Promise<Picked | null> {
    waiting?.abort();
    const controller = new AbortController();
    waiting = controller;
    const asked = await post("/login/passkey/options");
    if (follow(asked) || !asked.options) return null;
    const request: CredentialRequestOptions = {
      publicKey: requestOptions(asked.options.publicKey),
      signal: controller.signal,
    };
    if (conditional) request.mediation = "conditional";
    const credential = await navigator.credentials.get(request);
    return credential ? { token: asked.token, credential } : null;
  }

  async function logIn(picked: Picked): Promise<void> {
    const done = await post("/login/passkey", {
      token: picked.token,
      credential: credentialJSON(picked.credential as PublicKeyCredential),
      next,
    });
    follow(done);
  }

  // Among the name field's suggestions, where the browser can: the
  // request waits until a passkey is picked there. Picking none isn't a
  // problem, and neither is the site failing to start (the button says
  // why); a passkey the site refused is, and then the others are offered
  // again.
  function offer(): void {
    pick(true)
      .then((picked) =>
        picked
          ? logIn(picked).catch((e: unknown) => {
              if (!(e instanceof Refusal)) throw e;
              show(error, e.message);
              offer();
            })
          : undefined,
      )
      .catch(() => undefined);
  }
  void PublicKeyCredential.isConditionalMediationAvailable?.().then((available) => {
    if (available) offer();
  });

  button?.addEventListener("click", () => {
    show(error, null);
    pick(false)
      .then((picked) => (picked ? logIn(picked) : undefined))
      .catch((e: unknown) => show(error, problem(e)));
  });
}

function enableSecondFactor(box: HTMLElement): void {
  const error = box.querySelector<HTMLElement>("[data-passkey-error]");
  const button = box.querySelector<HTMLButtonElement>("button");
  box.hidden = false;
  button?.addEventListener("click", () => {
    show(error, null);
    (async () => {
      const asked = await post("/login/code/passkey/options");
      if (follow(asked) || !asked.options) return;
      const credential = await navigator.credentials.get({ publicKey: requestOptions(asked.options.publicKey) });
      if (!credential) return;
      const done = await post("/login/code/passkey", {
        token: asked.token,
        credential: credentialJSON(credential as PublicKeyCredential),
      });
      follow(done);
    })().catch((e: unknown) => show(error, problem(e)));
  });
}

export function enablePasskeys(root: Document = document): void {
  if (!supported()) return;
  const form = root.querySelector<HTMLFormElement>("form[data-passkey-register]");
  if (form) enableRegistration(form, root);
  const login = root.querySelector<HTMLElement>("[data-passkey-login]");
  if (login) enableLogin(login);
  const secondFactor = root.querySelector<HTMLElement>("[data-passkey-second-factor]");
  if (secondFactor) enableSecondFactor(secondFactor);
}
