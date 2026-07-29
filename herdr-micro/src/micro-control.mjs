import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";

const PLUGIN_ID = "gjermundgaraba.herdr-micro";
const stateDir =
  process.env.HERDR_PLUGIN_STATE_DIR ??
  path.join(os.homedir(), ".local", "state", "herdr", "plugins", PLUGIN_ID);

export const CONTROL_SOCKET = path.join(stateDir, "micro.sock");
export const LOG_FILE = path.join(stateDir, "micro.log");

export function ensureStateDir() {
  fs.mkdirSync(stateDir, { recursive: true });
}

export function requestDaemon(command, timeoutMs = 2_000) {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection(CONTROL_SOCKET);
    let buffer = "";
    const timer = setTimeout(() => {
      socket.destroy();
      reject(new Error("Micro bridge timed out"));
    }, timeoutMs);
    const finish = (callback) => {
      clearTimeout(timer);
      socket.destroy();
      callback();
    };
    socket.setEncoding("utf8");
    socket.once("connect", () => {
      socket.write(`${JSON.stringify({ command })}\n`);
    });
    socket.on("data", (chunk) => {
      buffer += chunk;
      const newline = buffer.indexOf("\n");
      if (newline < 0) return;
      finish(() => resolve(JSON.parse(buffer.slice(0, newline))));
    });
    socket.once("error", (error) => finish(() => reject(error)));
  });
}

function identity() {
  const stat = fs.statSync(CONTROL_SOCKET, { throwIfNoEntry: false });
  return stat ? `${stat.dev}:${stat.ino}` : null;
}

function bind(server) {
  return new Promise((resolve, reject) => {
    const onError = (error) => {
      server.off("listening", onListening);
      reject(error);
    };
    const onListening = () => {
      server.off("error", onError);
      resolve();
    };
    server.once("error", onError);
    server.once("listening", onListening);
    server.listen(CONTROL_SOCKET);
  });
}

export async function listenForControl(getStatus, stop) {
  ensureStateDir();
  const createServer = () =>
    net.createServer((socket) => {
      socket.setEncoding("utf8");
      let buffer = "";
      socket.on("data", (chunk) => {
        buffer += chunk;
        const newline = buffer.indexOf("\n");
        if (newline < 0) return;
        let command;
        try {
          command = JSON.parse(buffer.slice(0, newline)).command;
        } catch {
          socket.end('{"error":"invalid request"}\n');
          return;
        }
        if (command === "status") {
          socket.end(`${JSON.stringify(getStatus())}\n`);
        } else if (command === "stop") {
          socket.end('{"stopping":true}\n', stop);
        } else {
          socket.end('{"error":"unknown command"}\n');
        }
      });
    });

  let server = createServer();
  try {
    await bind(server);
  } catch (error) {
    if (error.code !== "EADDRINUSE") throw error;
    const before = identity();
    try {
      await requestDaemon("status", 500);
      throw new Error("Micro bridge is already running");
    } catch (probeError) {
      if (probeError.message === "Micro bridge is already running") {
        throw probeError;
      }
    }
    if (!before || identity() !== before) {
      throw new Error("Micro bridge socket changed during startup");
    }
    fs.rmSync(CONTROL_SOCKET);
    server = createServer();
    await bind(server);
  }
  const owned = identity();

  return () => {
    server.close();
    if (identity() === owned) fs.rmSync(CONTROL_SOCKET, { force: true });
  };
}
