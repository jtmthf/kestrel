import type { components, paths } from "./schema.ts";

export type Instruction = components["schemas"]["Instruction"];
export type Report = components["schemas"]["Report"];

export interface Delivered {
  readonly id: string;
  readonly instruction: Instruction;
}

/** The link declined this Environment. Reconnecting with the same credential will not help. */
export class Refused extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "Refused";
  }
}

const instructions = "/link/runs/{run}/instructions" satisfies keyof paths;
const reports = "/link/runs/{run}/reports" satisfies keyof paths;

export class Link {
  constructor(
    private readonly base: string,
    private readonly run: string,
    private readonly credential: string,
  ) {}

  async report(report: Report): Promise<void> {
    const response = await fetch(this.url(reports), {
      method: "POST",
      headers: { ...this.authorization(), "content-type": "application/json" },
      body: JSON.stringify(report),
    });

    await refuseIfDeclined(response);
    if (!response.ok) {
      throw new Error(`the link answered ${response.status} to a report`);
    }
  }

  async open(cursor?: string): Promise<AsyncIterable<Delivered>> {
    const response = await fetch(this.url(instructions), {
      headers: {
        ...this.authorization(),
        accept: "text/event-stream",
        ...(cursor === undefined ? {} : { "last-event-id": cursor }),
      },
    });

    await refuseIfDeclined(response);
    if (!response.ok || response.body === null) {
      throw new Error(`the link answered ${response.status} to a stream`);
    }

    return delivered(response.body);
  }

  private url(path: string): string {
    return `${this.base}${path.replace("{run}", encodeURIComponent(this.run))}`;
  }

  private authorization(): Record<string, string> {
    return { authorization: `Bearer ${this.credential}` };
  }
}

async function refuseIfDeclined(response: Response): Promise<void> {
  if (response.status !== 401 && response.status !== 403 && response.status !== 404) {
    return;
  }

  const refusal = await response.text();
  throw new Refused(response.status, refusal);
}

async function* delivered(body: ReadableStream<Uint8Array>): AsyncGenerator<Delivered> {
  for await (const frame of frames(body)) {
    let id: string | undefined;
    let data = "";

    for (const line of frame.split("\n")) {
      if (line.startsWith("id:")) {
        id = line.slice("id:".length).trim();
      } else if (line.startsWith("data:")) {
        data += line.slice("data:".length).trim();
      }
    }

    if (id !== undefined && data !== "") {
      yield { id, instruction: JSON.parse(data) as Instruction };
    }
  }
}

/** A frame is everything up to a blank line; the keep-alive is a frame with only a comment. */
async function* frames(body: ReadableStream<Uint8Array>): AsyncGenerator<string> {
  const decoder = new TextDecoder();
  let buffered = "";

  for await (const chunk of body) {
    buffered += decoder.decode(chunk, { stream: true });

    for (let end = buffered.indexOf("\n\n"); end !== -1; end = buffered.indexOf("\n\n")) {
      yield buffered.slice(0, end);
      buffered = buffered.slice(end + 2);
    }
  }
}
