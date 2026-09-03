import { expect, test } from '@playwright/test';
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import net from 'node:net';
import { RustChanServer } from './helpers';

async function portAcceptsConnections(port: number): Promise<boolean> {
  return new Promise<boolean>((resolve) => {
    const socket = net.createConnection({ host: '127.0.0.1', port });
    let settled = false;
    const finish = (connected: boolean) => {
      if (settled) {
        return;
      }
      settled = true;
      socket.destroy();
      resolve(connected);
    };
    socket.once('connect', () => finish(true));
    socket.once('error', () => finish(false));
    socket.setTimeout(1_000, () => finish(false));
  });
}

test.describe('RustChan fixture lifecycle', () => {
  test.beforeEach(async ({}, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'fixture lifecycle runs once in Chromium');
  });

  test('dispose removes temporary roots and supports explicit preservation', async () => {
    const disposable = await RustChanServer.create();
    const disposableRoot = disposable.rootDir;
    try {
      await disposable.dispose();
      expect(fs.existsSync(disposableRoot)).toBe(false);
    } finally {
      await fsp.rm(disposableRoot, { recursive: true, force: true });
    }

    const preserved = await RustChanServer.create(undefined, { preserveRoot: true });
    const preservedRoot = preserved.rootDir;
    try {
      await preserved.dispose();
      expect(fs.existsSync(preservedRoot)).toBe(true);
    } finally {
      await fsp.rm(preservedRoot, { recursive: true, force: true });
    }
  });

  test('dispose stops a started process before removing its root', async () => {
    const app = await RustChanServer.create();
    const rootDir = app.rootDir;
    try {
      await app.start();
      const child = app.process;
      if (!child) {
        throw new Error('RustChan fixture did not retain its started child process');
      }
      expect(await portAcceptsConnections(app.port)).toBe(true);

      await app.dispose();

      expect(app.process).toBeUndefined();
      // Headless fixtures shut down through SIGTERM, not the TUI's Q/Enter
      // confirmation. An escalation to SIGKILL must not pass this regression.
      expect(child.exitCode).toBe(0);
      expect(child.signalCode).toBeNull();
      expect(await portAcceptsConnections(app.port)).toBe(false);
      expect(fs.existsSync(rootDir)).toBe(false);
    } finally {
      if (app.process) {
        await app.dispose();
      }
      if (!app.process) {
        await fsp.rm(rootDir, { recursive: true, force: true });
      }
    }
  });
});
