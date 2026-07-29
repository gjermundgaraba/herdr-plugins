import { requestDaemon } from "./micro-control.mjs";

const command = process.argv[2];
if (!["status", "stop"].includes(command)) {
  throw new Error("expected status or stop");
}

console.log(JSON.stringify(await requestDaemon(command), null, 2));
