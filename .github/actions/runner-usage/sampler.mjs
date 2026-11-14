// Appends one line per interval to the file in argv[2]:
//   {"t": seconds, "busy": 0..1, "io": 0..1, "mem": bytes in use}
// busy is the share of all CPUs doing work over the interval; io the share
// idle while waiting on disk (Linux only, 0 elsewhere).
import { appendFileSync, readFileSync } from "node:fs";
import os from "node:os";

const file = process.argv[2];
const INTERVAL_MS = 2000;

// Linux: /proc/stat has iowait, which os.cpus() drops. In a container it still
// shows the whole machine, which is what the runner size is about.
function cpuTimes() {
  try {
    const line = readFileSync("/proc/stat", "utf8").split("\n")[0];
    const [user, nice, system, idle, iowait, irq, softirq, steal] = line.trim().split(/\s+/).slice(1).map(Number);
    return { busy: user + nice + system + irq + softirq + steal, idle, io: iowait };
  } catch {
    let busy = 0, idle = 0;
    for (const { times: t } of os.cpus()) {
      busy += t.user + t.nice + t.sys + t.irq;
      idle += t.idle;
    }
    return { busy, idle, io: 0 };
  }
}

function memUsed() {
  try {
    const m = Object.fromEntries(
      readFileSync("/proc/meminfo", "utf8").split("\n").filter(Boolean)
        .map((l) => { const [k, v] = l.split(":"); return [k, parseInt(v, 10) * 1024]; }),
    );
    return m.MemTotal - m.MemAvailable;
  } catch {
    return os.totalmem() - os.freemem();
  }
}

let prev = cpuTimes();
const t0 = Date.now();
setInterval(() => {
  const cur = cpuTimes();
  const busy = cur.busy - prev.busy, idle = cur.idle - prev.idle, io = cur.io - prev.io;
  const total = busy + idle + io;
  prev = cur;
  if (total <= 0) return;
  const sample = { t: (Date.now() - t0) / 1000, busy: busy / total, io: io / total, mem: memUsed() };
  appendFileSync(file, JSON.stringify(sample) + "\n");
}, INTERVAL_MS);
