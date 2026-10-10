import assert from "node:assert/strict";
import { test } from "node:test";

import { type Answer, MAX_TRIES, type Transport, TransferError, metadata, pieceSize, sendFile, smaller, tokenOf } from "./transfer.ts";

const MB = 1024 * 1024;

test("the first piece is small, later ones take about ten seconds, within the server's limit", () => {
  assert.equal(pieceSize(null, null, 50 * MB), 4 * MB);
  assert.equal(pieceSize(null, null, 1 * MB), 1 * MB);
  // 2 MB/s: 20 MB, but no more than four times the last piece.
  assert.equal(pieceSize(4 * MB, 2 * MB, 50 * MB), 16 * MB);
  assert.equal(pieceSize(16 * MB, 2 * MB, 50 * MB), 20 * MB);
  // A fast connection, held to the server's limit.
  assert.equal(pieceSize(40 * MB, 100 * MB, 50 * MB), 50 * MB);
  // A slow one, not below the smallest piece.
  assert.equal(pieceSize(4 * MB, 1000, 50 * MB), 256 * 1024);
  // A limit below the smallest piece wins.
  assert.equal(pieceSize(4, 1000, 4), 4);
});

test("a failed piece is sent again at half its size, down to the smallest", () => {
  assert.equal(smaller(8 * MB), 4 * MB);
  assert.equal(smaller(300 * 1024), 256 * 1024);
  assert.equal(smaller(256 * 1024), 256 * 1024);
  assert.equal(smaller(4), 4);
});

test("the file's name goes in base64, as UTF-8", () => {
  assert.equal(metadata("a.png"), "filename YS5wbmc=");
  assert.equal(metadata("猫.png"), `filename ${Buffer.from("猫.png").toString("base64")}`);
});

test("the token is the end of the transfer's URL", () => {
  assert.equal(tokenOf("/uploads/files/abc123"), "abc123");
  assert.equal(tokenOf("https://booru.example/uploads/files/abc123"), "abc123");
});

const answer = (status: number, headers: Record<string, string> = {}, text = ""): Answer => ({
  status,
  header: (name) => headers[name.toLowerCase()] ?? null,
  text,
});

/** A server keeping one transfer, answering pieces with `failures` first. */
function server(failures: number[] = []): Transport & { bytes: string; pieces: number[]; requests: string[]; created: Record<string, string> } {
  const fake = {
    bytes: "",
    pieces: [] as number[],
    requests: [] as string[],
    created: {} as Record<string, string>,
    async request(method: string, url: string, headers: Record<string, string>): Promise<Answer> {
      fake.requests.push(`${method} ${new URL(url).pathname}`);
      if (method === "POST") {
        fake.created = headers;
        return answer(201, { location: "/uploads/files/tok" });
      }
      if (method === "HEAD") return answer(200, { "upload-offset": String(fake.bytes.length) });
      return answer(204);
    },
    async piece(url: string, body: Blob, headers: Record<string, string>, sent: (bytes: number) => void): Promise<Answer> {
      fake.requests.push(`PATCH ${new URL(url).pathname} @${headers["Upload-Offset"]}`);
      const failure = failures.shift();
      if (failure !== undefined) return answer(failure, {}, failure === 422 ? "That isn't an image." : "<html>error</html>");
      assert.equal(headers["Content-Type"], "application/offset+octet-stream");
      assert.equal(Number(headers["Upload-Offset"]), fake.bytes.length);
      const text = await body.text();
      sent(text.length);
      fake.bytes += text;
      fake.pieces.push(text.length);
      return answer(204, { "upload-offset": String(fake.bytes.length) });
    },
  };
  return fake;
}

const options = (transport: Transport, extra = {}) => ({
  maxPiece: 4,
  base: "https://booru.example/uploads/new",
  transport,
  wait: async () => undefined,
  ...extra,
});

test("a file goes in pieces, and its token comes back", async () => {
  const fake = server();
  const seen: number[] = [];
  const token = await sendFile(new Blob(["image bytes"]), "cat.png", options(fake, { progress: (n: number) => seen.push(n) }));
  assert.equal(token, "tok");
  assert.equal(fake.bytes, "image bytes");
  assert.deepEqual(fake.pieces, [4, 4, 3]);
  assert.equal(fake.created["Upload-Length"], "11");
  assert.equal(fake.created["Upload-Metadata"], metadata("cat.png"));
  assert.equal(fake.created["Tus-Resumable"], "1.0.0");
  assert.equal(seen.at(-1), 11);
});

test("a failed piece is sent again from where the server says", async () => {
  // No answer, a proxy refusing the piece as too large, a timeout.
  const fake = server([0, 413, 408]);
  let retries = 0;
  await sendFile(new Blob(["image bytes"]), "cat.png", options(fake, { retrying: () => retries++ }));
  assert.equal(fake.bytes, "image bytes");
  assert.equal(retries, 3);
  assert.equal(fake.requests.filter((r) => r.startsWith("HEAD")).length, 3);
});

test("a piece failing too often gives the file up, removing what was sent", async () => {
  const fake = server(Array(MAX_TRIES).fill(502));
  await assert.rejects(sendFile(new Blob(["image bytes"]), "cat.png", options(fake)), (error: unknown) => {
    // A proxy's error page isn't shown as the reason.
    assert.ok(error instanceof TransferError);
    assert.equal(error.message, "");
    return true;
  });
  assert.equal(fake.requests.at(-1), "DELETE /uploads/files/tok");
});

test("a refusal says why at once", async () => {
  const fake = server([422]);
  await assert.rejects(sendFile(new Blob(["image bytes"]), "cat.png", options(fake)), new TransferError("That isn't an image."));
  assert.equal(fake.requests.filter((r) => r.startsWith("PATCH")).length, 1);
  assert.equal(fake.requests.at(-1), "DELETE /uploads/files/tok");
});

test("a file the server won't take isn't sent", async () => {
  const fake = server();
  fake.request = async (method) => {
    fake.requests.push(method);
    return answer(413, {}, "The file is larger than 100 MB.");
  };
  await assert.rejects(sendFile(new Blob(["image bytes"]), "cat.png", options(fake)), new TransferError("The file is larger than 100 MB."));
  assert.deepEqual(fake.requests, ["POST"]);
});
