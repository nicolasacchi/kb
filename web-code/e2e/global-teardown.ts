export default async function globalTeardown() {
  const daemon = globalThis.__KB_CODE_DAEMON__;
  if (daemon) {
    try {
      daemon.process.kill("SIGTERM");
    } catch {
      // ignore
    }
  }
  // DCB W2.B — the doc-lens mock (`doclens-fixture.ts`), started in
  // global-setup.ts before the daemon.
  const doclensFixture = globalThis.__DOCLENS_FIXTURE__;
  if (doclensFixture) {
    try {
      doclensFixture.close();
    } catch {
      // ignore
    }
  }
}
