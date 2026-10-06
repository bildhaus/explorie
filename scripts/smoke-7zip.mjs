import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { SEVENZIP_VERSION } from './prepare-7zip.mjs';

export function verifySevenZipVersion(output) {
  // The standalone Unix engine includes the (z) variant in its banner.
  const version = /^7-Zip(?: \(z\))? (\d+\.\d+)(?= )/m.exec(output)?.[1];
  assert.equal(version, SEVENZIP_VERSION, 'Unexpected 7-Zip version');
}

export async function smokeSevenZip(executable) {
  executable = path.resolve(executable);
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'explorie-7zip-smoke-'));
  const run = (...args) => execFileSync(executable, args, { cwd: temporary, encoding: 'utf8', windowsHide: true });
  try {
    const formats = run('i');
    verifySevenZipVersion(formats);
    for (const format of ['Cab', 'Dmg', 'Iso', 'Rar', 'Wim']) {
      assert.match(formats, new RegExp(`\\s${format}\\s`, 'i'), `Full engine must support ${format}`);
    }
    const payload = 'Explorie bundled archive round-trip\n';
    await writeFile(path.join(temporary, 'payload.txt'), payload);
    // XZ exercises a format outside the previous ZIP/TAR/RAR/7z dispatcher.
    run('a', '-txz', 'payload.xz', 'payload.txt');
    run('t', 'payload.xz');
    run('x', 'payload.xz', '-oextracted', '-y');
    assert.equal(await readFile(path.join(temporary, 'extracted', 'payload'), 'utf8'), payload);
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (!process.argv[2]) throw new Error('Usage: node scripts/smoke-7zip.mjs <executable>');
  await smokeSevenZip(process.argv[2]);
  console.log('Bundled full 7-Zip engine passed archive smoke.');
}
