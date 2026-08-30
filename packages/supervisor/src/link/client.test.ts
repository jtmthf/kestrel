import { afterEach, expect, test } from "bun:test";

import { Link, Refused } from "./client.ts";

interface Recorded {
  path: string;
  headers: Headers;
}

let server: ReturnType<typeof Bun.serve> | undefined;

afterEach(() => {
  server?.stop(true);
  server = undefined;
});

function link_(answer: (request: Request) => Response): { link: Link; asked: Recorded[] } {
  const asked: Recorded[] = [];

  server = Bun.serve({
    port: 0,
    fetch(request) {
      asked.push({ path: new URL(request.url).pathname, headers: request.headers });
      return answer(request);
    },
  });

  return { link: new Link(server.url.origin, "a-run", "a-credential"), asked };
}

function stream(body: string): Response {
  return new Response(body, { headers: { "content-type": "text/event-stream" } });
}

test("opening the stream presents the run's credential", async () => {
  const { link, asked } = link_(() => stream(""));

  await link.open();

  expect(asked[0]?.path).toBe("/link/runs/a-run/instructions");
  expect(asked[0]?.headers.get("authorization")).toBe("Bearer a-credential");
});

test("a first connection carries no cursor, and a reconnection carries the one it holds", async () => {
  const { link, asked } = link_(() => stream(""));

  await link.open();
  await link.open("3");

  expect(asked[0]?.headers.get("last-event-id")).toBeNull();
  expect(asked[1]?.headers.get("last-event-id")).toBe("3");
});

test("the instructions delivered are the ones the stream carried", async () => {
  const { link } = link_(() =>
    stream('id: 4\nevent: stop\ndata: {"kind":"stop"}\n\n:\n\nid: 5\nevent: stop\ndata: {"kind":"stop"}\n\n'),
  );

  const delivered = [];
  for await (const instruction of await link.open()) {
    delivered.push(instruction);
  }

  expect(delivered).toEqual([
    { id: "4", instruction: { kind: "stop" } },
    { id: "5", instruction: { kind: "stop" } },
  ]);
});

test("a refused credential is not something to reconnect through", async () => {
  const { link } = link_(() => new Response('{"message":"no credential kestrel issued"}', { status: 401 }));

  await expect(link.open()).rejects.toBeInstanceOf(Refused);
  await expect(
    link.report({ kind: "connected", version: "0.0.0" }),
  ).rejects.toBeInstanceOf(Refused);
});

test("reporting posts to the run's reports", async () => {
  const { link, asked } = link_(() => new Response(null, { status: 202 }));

  await link.report({ kind: "connected", version: "0.0.0" });

  expect(asked[0]?.path).toBe("/link/runs/a-run/reports");
});
