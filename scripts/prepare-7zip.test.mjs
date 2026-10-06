import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { SEVENZIP_SOURCE, SEVENZIP_TARGETS, SEVENZIP_VERSION, prepareSevenZip, verifySha256 } from './prepare-7zip.mjs';
import { verifySevenZipVersion } from './smoke-7zip.mjs';

test('7-Zip version smoke accepts Windows and standalone Unix banners only at the pinned version', () => {
  for (const banner of ['7-Zip 26.03 (x64)', '7-Zip (z) 26.03 (arm64)']) {
    assert.doesNotThrow(() => verifySevenZipVersion(`\n${banner} : Copyright (c) Igor Pavlov\n`));
  }
  for (const banner of ['7-Zip (z) 25.01 (arm64)', '7-Zip 26.030 (x64)', '7-Zip (a) 26.03 (arm64)', 'p7zip 26.03', 'unknown']) {
    assert.throws(() => verifySevenZipVersion(banner), /Unexpected 7-Zip version/);
  }
});

test('7-Zip download integrity rejects altered bytes', () => {
  const digest = 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad';
  assert.doesNotThrow(() => verifySha256(Buffer.from('abc'), digest, 'fixture'));
  assert.throws(() => verifySha256(Buffer.from('abd'), digest, 'fixture'), /SHA-256 mismatch for fixture/);
});

test('installed corresponding source verification rejects altered bytes and missing paths', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'explorie-7zip-source-test-'));
  const script = fileURLToPath(new URL('./prepare-7zip.mjs', import.meta.url));
  try {
    const archive = path.join(directory, SEVENZIP_SOURCE.archive);
    await writeFile(archive, 'altered source');
    const altered = spawnSync(process.execPath, [script, '--verify-source', archive], { encoding: 'utf8' });
    assert.notEqual(altered.status, 0);
    assert.match(altered.stderr, /SHA-256 mismatch/);
    for (const flag of ['--stage-source', '--verify-source']) {
      const missing = spawnSync(process.execPath, [script, flag], { encoding: 'utf8' });
      assert.notEqual(missing.status, 0);
      assert.match(missing.stderr, /requires a path/);
    }
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('full engine and corresponding source are pinned for supported targets', async () => {
  assert.equal(SEVENZIP_VERSION, '26.03');
  assert.equal(SEVENZIP_SOURCE.archive, '7z2603-src.tar.xz');
  assert.match(SEVENZIP_SOURCE.sha256, /^[a-f0-9]{64}$/);
  assert.equal(Object.keys(SEVENZIP_TARGETS).length, 4);
  for (const [target, asset] of Object.entries(SEVENZIP_TARGETS)) {
    assert.match(asset.sha256, /^[a-f0-9]{64}$/);
    assert.deepEqual(Object.keys(asset.files), target.includes('windows') ? ['7z.exe', '7z.dll'] : ['7zz']);
    for (const digest of Object.values(asset.files)) assert.match(digest, /^[a-f0-9]{64}$/);
  }
  await assert.rejects(prepareSevenZip('unknown-target'), /Unsupported 7-Zip sidecar target/);
});

test('7-Zip licenses, corresponding source, packaged paths and signing remain connected', async () => {
  const read = (file) => readFile(new URL(`../${file}`, import.meta.url), 'utf8');
  const [notice, license, copying, installer, windows, macos, release] = await Promise.all([
    'apps/desktop/native-assets/resources/7zip-NOTICE.txt',
    'apps/desktop/native-assets/resources/7zip-LICENSE.txt',
    'apps/desktop/native-assets/resources/7zip-COPYING.txt',
    'apps/desktop/gpui/installer/windows/explorie.iss',
    'scripts/package-gpui-windows.ps1',
    'scripts/package-gpui-macos.sh',
    '.github/workflows/build-release.yml',
  ].map(read));
  assert.ok(notice.includes(SEVENZIP_VERSION));
  assert.ok(notice.includes(SEVENZIP_SOURCE.archive));
  assert.ok(notice.includes(SEVENZIP_SOURCE.sha256));
  assert.match(license, /unRAR license restriction/);
  assert.match(copying, /GNU LESSER GENERAL PUBLIC LICENSE/);
  for (const file of ['7zip-LICENSE.txt', '7zip-COPYING.txt', '7zip-NOTICE.txt']) {
    assert.ok(installer.includes(file));
    assert.ok(macos.includes(file));
  }
  assert.match(installer, /7zip\\7z\.dll.*DestDir: "\{app\}\\7zip"/);
  assert.ok(installer.includes(`licenses\\${SEVENZIP_SOURCE.archive}"; DestDir: "{app}\\licenses"`));
  assert.match(windows, /prepare-7zip\.mjs"\) --stage-source \(Join-Path \$build "licenses"\)/);
  assert.match(macos, /prepare-7zip\.mjs" --stage-source "\$app\/Contents\/Resources\/licenses"/);
  assert.match(macos, /sign com\.omershatz\.explorie\.sevenzip "\$app\/Contents\/Resources\/7zip\/7zz"/);
  assert.match(release, /smoke-7zip\.mjs \(Join-Path \$installDir "7zip\/7z\.exe"\)/);
  assert.match(release, /smoke-7zip\.mjs "\$installed_app\/Contents\/Resources\/7zip\/7zz"/);
});
