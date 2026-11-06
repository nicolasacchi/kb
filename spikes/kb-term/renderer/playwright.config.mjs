import { defineConfig } from '@playwright/test';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  testDir: path.join(here, 'tests'),
  outputDir: path.join(here, 'out', 'pw-artifacts'),
  timeout: 120000,
  workers: 2,
  retries: 0,
  reporter: [['list'], ['json', { outputFile: path.join(here, 'out', 'pw.json') }]],
  use: {
    browserName: 'chromium',
    launchOptions: { args: ['--use-angle=swiftshader', '--use-gl=angle', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist', '--enable-webgl'] },
  },
  webServer: { command: 'node server.mjs', cwd: here, url: 'http://127.0.0.1:4173/', reuseExistingServer: false, timeout: 30000 },
});
