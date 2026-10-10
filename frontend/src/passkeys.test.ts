import assert from "node:assert/strict";
import { test } from "node:test";

import {
  Refusal,
  creationOptions,
  credentialJSON,
  fromBase64url,
  problem,
  requestOptions,
  toBase64url,
} from "./passkeys.ts";

const bytes = (...values: number[]) => new Uint8Array(values);

test("bytes go to unpadded base64url and back", () => {
  // 0xfb 0xff needs both of base64url's own characters.
  assert.equal(toBase64url(bytes(0xfb, 0xff)), "-_8");
  assert.equal(toBase64url(bytes(1, 2, 3).buffer), "AQID");
  assert.equal(toBase64url(bytes()), "");
  assert.deepEqual([...fromBase64url("-_8")], [0xfb, 0xff]);
  // Padded and standard base64 read the same.
  assert.deepEqual([...fromBase64url("+/8=")], [0xfb, 0xff]);
  const random = Uint8Array.from({ length: 33 }, (_, i) => (i * 37) % 256);
  assert.deepEqual(fromBase64url(toBase64url(random)), random);
});

test("registration options get their binary fields as bytes", () => {
  const options = creationOptions({
    rp: { id: "example.com", name: "example.com" },
    user: { id: "AQID", name: "alice", displayName: "alice" },
    challenge: "BAUG",
    pubKeyCredParams: [{ type: "public-key", alg: -7 }],
    excludeCredentials: [{ type: "public-key", id: "Bwg" }],
    authenticatorSelection: { residentKey: "preferred", userVerification: "required" },
  });
  assert.deepEqual([...(options.challenge as Uint8Array)], [4, 5, 6]);
  assert.deepEqual([...(options.user.id as Uint8Array)], [1, 2, 3]);
  assert.equal(options.user.name, "alice");
  assert.deepEqual([...(options.excludeCredentials![0]!.id as Uint8Array)], [7, 8]);
  assert.equal(options.authenticatorSelection?.residentKey, "preferred");
});

test("login options may name no passkeys", () => {
  const any = requestOptions({ challenge: "AQID", rpId: "example.com", userVerification: "required" });
  assert.deepEqual([...(any.challenge as Uint8Array)], [1, 2, 3]);
  assert.equal(any.allowCredentials, undefined);
  const hers = requestOptions({ challenge: "AQID", allowCredentials: [{ type: "public-key", id: "CQ" }] });
  assert.deepEqual([...(hers.allowCredentials![0]!.id as Uint8Array)], [9]);
});

test("answers go to the server as base64url", () => {
  const base = {
    id: "Bwg",
    rawId: bytes(7, 8).buffer,
    type: "public-key",
    getClientExtensionResults: () => ({ credProps: { rk: true } }),
  };
  const made = credentialJSON({
    ...base,
    response: {
      clientDataJSON: bytes(1).buffer,
      attestationObject: bytes(2).buffer,
      getTransports: () => ["internal", "hybrid"],
    },
  } as unknown as PublicKeyCredential);
  assert.deepEqual(made, {
    id: "Bwg",
    rawId: "Bwg",
    type: "public-key",
    response: { clientDataJSON: "AQ", attestationObject: "Ag", transports: ["internal", "hybrid"] },
    clientExtensionResults: { credProps: { rk: true } },
  });

  const signed = credentialJSON({
    ...base,
    response: {
      clientDataJSON: bytes(1).buffer,
      authenticatorData: bytes(3).buffer,
      signature: bytes(4).buffer,
      userHandle: null,
    },
  } as unknown as PublicKeyCredential);
  assert.deepEqual(signed["response"], {
    clientDataJSON: "AQ",
    authenticatorData: "Aw",
    signature: "BA",
    userHandle: null,
  });
});

test("the person hears why, except when the script stopped it", () => {
  const named = (name: string) => Object.assign(new Error("browser text"), { name });
  assert.equal(problem(named("AbortError")), null);
  assert.match(problem(named("NotAllowedError"))!, /No passkey was used/);
  assert.match(problem(named("InvalidStateError"))!, /already has a passkey/);
  assert.equal(problem(new Refusal("That passkey didn't work.")), "That passkey didn't work.");
  assert.match(problem(named("SecurityError"))!, /Something went wrong/);
  assert.match(problem("odd")!, /Something went wrong/);
});
