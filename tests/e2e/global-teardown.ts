export default async function globalTeardown() {
  const daemon = globalThis.__KB_DAEMON__;
  if (!daemon) return;
  try {
    daemon.process.kill("SIGTERM");
  } catch {
    // ignore
  }
}
