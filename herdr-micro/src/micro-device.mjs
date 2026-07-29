import { spawn } from "node:child_process";
import { once } from "node:events";
import { fileURLToPath } from "node:url";
import { encodeMessage, Reassembler } from "./micro-protocol.mjs";

const helper = fileURLToPath(new URL("../bin/micro-hid", import.meta.url));

export function deviceEvent(method, params) {
  if (method === "v.oai.hid") {
    return {
      type: "key",
      key: String(params?.k ?? ""),
      action: Number(params?.act ?? 0),
    };
  }
  if (method === "v.oai.rad") {
    return {
      type: "joystick",
      angle: Number(params?.a ?? 0),
      distance: Number(params?.d ?? 0),
    };
  }
  return null;
}

export class MicroDevice {
  #child;
  #closed = false;
  #onError;
  #opened;
  #ready = false;
  #openResolve;
  #openReject;
  #reassembler = new Reassembler();
  #requestId = 0;
  #responses = new Map();
  #writeId = 0;
  #writeResponses = new Map();
  #writes = Promise.resolve();

  constructor(child, onEvent, onError) {
    this.#child = child;
    this.#onError = onError;
    this.#opened = new Promise((resolve, reject) => {
      this.#openResolve = resolve;
      this.#openReject = reject;
    });

    let stdout = "";
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
      let newline;
      while ((newline = stdout.indexOf("\n")) >= 0) {
        const line = stdout.slice(0, newline);
        stdout = stdout.slice(newline + 1);
        if (!line) continue;
        try {
          const message = JSON.parse(line);
          if (message.type === "ready") {
            this.#ready = true;
            this.#openResolve(message.transport);
          } else if (message.type === "wrote") {
            this.#settle(this.#writeResponses, message.id);
          } else if (message.type === "error") {
            const error = new Error(message.message);
            if (message.id !== undefined) {
              this.#settle(this.#writeResponses, message.id, error);
            } else {
              this.#fail(error);
            }
          } else if (message.type === "data") {
            for (const text of this.#reassembler.push(
              Buffer.from(message.report, "base64"),
            )) {
              const envelope = JSON.parse(text);
              const response = this.#responses.get(envelope.id);
              if (response && ("result" in envelope || "error" in envelope)) {
                this.#responses.delete(envelope.id);
                clearTimeout(response.timer);
                if (envelope.error) {
                  response.reject(
                    new Error(
                      envelope.error.message ?? `request ${envelope.id} failed`,
                    ),
                  );
                } else {
                  response.resolve(envelope.result);
                }
              } else {
                const method = envelope.m ?? envelope.method;
                const params = envelope.p ?? envelope.params;
                const event = deviceEvent(method, params);
                if (event) onEvent(event);
              }
            }
          }
        } catch {
          // Ignore malformed or unrelated helper/device messages.
        }
      }
    });
    child.once("error", (error) => this.#fail(error));
    child.once("exit", (code) => {
      if (!this.#closed) {
        this.#fail(new Error(`Micro HID helper exited (${code ?? "signal"})`));
      }
    });
  }

  static async open(onEvent, onError) {
    const child = spawn(helper, [], { stdio: ["pipe", "pipe", "ignore"] });
    const device = new MicroDevice(child, onEvent, onError);
    try {
      await device.#opened;
      await device.request("device.status");
      return device;
    } catch (error) {
      await device.close();
      throw error;
    }
  }

  #settle(pending, id, error) {
    const response = pending.get(id);
    if (!response) return;
    pending.delete(id);
    error ? response.reject(error) : response.resolve();
  }

  #fail(error) {
    this.#openReject(error);
    for (const pending of [
      ...this.#writeResponses.values(),
      ...this.#responses.values(),
    ]) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.#writeResponses.clear();
    this.#responses.clear();
    if (!this.#closed && this.#ready) this.#onError(error);
  }

  #send(method, params, requestId) {
    const target = this.#child;
    const write = this.#writes.then(async () => {
      if (this.#closed || this.#child !== target) {
        throw new Error("device disconnected");
      }
      for (const report of encodeMessage(method, params, requestId)) {
        const id = ++this.#writeId;
        const result = new Promise((resolve, reject) => {
          this.#writeResponses.set(id, { resolve, reject });
        });
        target.stdin.write(
          `${JSON.stringify({ id, write: report.toString("base64") })}\n`,
        );
        await result;
      }
    });
    this.#writes = write.catch(() => {});
    return write;
  }

  setLighting(slots) {
    return this.#send("v.oai.thstatus", slots);
  }

  setAggregateLighting(zones) {
    return this.#send("v.oai.rgbcfg", zones);
  }

  setFocusedApp(app) {
    return this.#send("host.focused_app", app);
  }

  async request(method, params, timeoutMs = 2_000) {
    const id = ++this.#requestId;
    let response;
    const result = new Promise((resolve, reject) => {
      response = {
        resolve,
        reject,
        timer: setTimeout(() => {
          this.#responses.delete(id);
          reject(new Error(`${method} timed out`));
        }, timeoutMs),
      };
      this.#responses.set(id, response);
    });
    try {
      await this.#send(method, params, id);
      return await result;
    } catch (error) {
      this.#responses.delete(id);
      clearTimeout(response.timer);
      throw error;
    }
  }

  async close() {
    if (this.#closed) return;
    this.#closed = true;
    const child = this.#child;
    this.#child = null;
    for (const pending of [
      ...this.#writeResponses.values(),
      ...this.#responses.values(),
    ]) {
      clearTimeout(pending.timer);
      pending.reject(new Error("device disconnected"));
    }
    this.#writeResponses.clear();
    this.#responses.clear();
    child.stdin.end();
    await Promise.race([
      once(child, "exit"),
      new Promise((resolve) => setTimeout(resolve, 500)),
    ]);
    if (child.exitCode === null) child.kill();
  }
}
