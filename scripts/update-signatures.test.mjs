import assert from 'node:assert/strict';
import { generateKeyPairSync, randomBytes, scryptSync } from 'node:crypto';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import {
  PUBLIC_KEY_FILE,
  main,
  parsePublicKey,
  parseSecretKey,
  parseSignatureLines,
  scryptParameters,
  signAsset,
  stripSignatureBlock,
  verifyAsset,
  withSignatureBlock,
} from './update-signatures.mjs';

// An independent minisign implementation (rsign2 0.6.7) signed the 1 MiB
// zero-filled payload with this test-only key; its secret half was discarded.
const RSIGN_PUBLIC_KEY = 'untrusted comment: minisign public key: 2036DC2ADF06AB2A\nRWQqqwbfKtw2IClva6awfwNN/7AzqRTTVAulQk++WRgrJZ0RD+XsBR0q\n';
const RSIGN_SIGNATURE = 'explorie-signature explorie-0.2.9-macos-arm64.dmg RUQqqwbfKtw2IJPoJiMeXyujyiSp3ul9o6REHxLg/9E5ZaMBebq6mzVf8m5d3dw/4C+KPzVzghOpZ+wjkqdGH7wd3Ywj/jeqkgo= +d7rrucTgXOacdm0Bvru90Qr4Qyz9J/fu80g07W9Hr5aHp/rOKN01tcn/Yt4hhnL7rQw9MDjMt8ZWAmOGWopAg==';

/** Build a minisign-format key pair; `password` encrypts the secret key. */
function minisignKeyPair(password) {
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const seed = privateKey.export({ format: 'der', type: 'pkcs8' }).subarray(16);
  const key = publicKey.export({ format: 'der', type: 'spki' }).subarray(12);
  const keyId = randomBytes(8);
  const salt = randomBytes(32);
  const opslimit = 1048576n;
  const memlimit = 33554432n;
  const keynum = Buffer.concat([keyId, seed, key, Buffer.alloc(32)]);
  if (password !== undefined) {
    const stream = scryptSync(password, salt, keynum.length, scryptParameters(opslimit, memlimit));
    for (let index = 0; index < keynum.length; index += 1) keynum[index] ^= stream[index];
  }
  const header = Buffer.alloc(54);
  header.write('Ed', 0, 'latin1');
  if (password !== undefined) header.write('Sc', 2, 'latin1');
  header.write('B2', 4, 'latin1');
  salt.copy(header, 6);
  header.writeBigUInt64LE(password !== undefined ? opslimit : 0n, 38);
  header.writeBigUInt64LE(password !== undefined ? memlimit : 0n, 46);
  return {
    secretText: `untrusted comment: test secret key\n${Buffer.concat([header, keynum]).toString('base64')}\n`,
    publicText: `untrusted comment: test public key\n${Buffer.concat([Buffer.from('Ed'), keyId, key]).toString('base64')}\n`,
  };
}

async function fixtureDirectory() {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'explorie-update-signatures-'));
  const windows = path.join(directory, 'explorie-1.2.3-windows-x64-setup-unsigned.exe');
  const macos = path.join(directory, 'explorie-1.2.3-macos-arm64.dmg');
  await writeFile(windows, randomBytes(256 * 1024));
  await writeFile(macos, randomBytes(256 * 1024));
  return { directory, windows, macos };
}

test('scrypt parameters follow libsodium for minisign and rsign key files', () => {
  assert.deepEqual(
    { ...scryptParameters(1048576n, 33554432n), maxmem: undefined },
    { N: 32768, r: 8, p: 1, maxmem: undefined },
  );
  assert.deepEqual(
    { ...scryptParameters(33554432n, 1073741824n), maxmem: undefined },
    { N: 1048576, r: 8, p: 1, maxmem: undefined },
  );
});

test('signatures from an independent minisign implementation verify', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'explorie-update-signatures-'));
  try {
    const payload = path.join(directory, 'explorie-0.2.9-macos-arm64.dmg');
    await writeFile(payload, Buffer.alloc(1024 * 1024));
    const publicKey = parsePublicKey(RSIGN_PUBLIC_KEY);
    const entry = parseSignatureLines(RSIGN_SIGNATURE).get('explorie-0.2.9-macos-arm64.dmg');
    await verifyAsset(payload, 'explorie-0.2.9-macos-arm64.dmg', entry, publicKey);
    await assert.rejects(verifyAsset(payload, 'explorie-0.3.0-macos-arm64.dmg', entry, publicKey), /invalid/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('plain and password-protected minisign keys sign verifiable asset lines', async () => {
  const { directory, windows, macos } = await fixtureDirectory();
  try {
    for (const password of [undefined, '', 'correct horse battery staple']) {
      const pair = minisignKeyPair(password);
      const publicKey = parsePublicKey(pair.publicText);
      const secret = parseSecretKey(pair.secretText, password ?? '');
      assert.ok(secret.keyId.equals(publicKey.keyId));
      const lines = [
        await signAsset(windows, path.basename(windows), secret),
        await signAsset(macos, path.basename(macos), secret),
      ];
      const signatures = parseSignatureLines(lines.join('\n'));
      await verifyAsset(windows, path.basename(windows), signatures.get(path.basename(windows)), publicKey);
      await verifyAsset(macos, path.basename(macos), signatures.get(path.basename(macos)), publicKey);
      if (password) assert.throws(() => parseSecretKey(pair.secretText, 'wrong'), /Wrong UPDATE_SIGNING_KEY_PASSWORD/);
    }
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('tampered, replayed, foreign and missing signatures are rejected', async () => {
  const { directory, windows, macos } = await fixtureDirectory();
  try {
    const pair = minisignKeyPair();
    const other = minisignKeyPair();
    const publicKey = parsePublicKey(pair.publicText);
    const secret = parseSecretKey(pair.secretText);
    const windowsName = path.basename(windows);
    const macosName = path.basename(macos);
    const entry = parseSignatureLines(await signAsset(windows, windowsName, secret)).get(windowsName);

    await assert.rejects(verifyAsset(macos, windowsName, entry, publicKey), /invalid/);
    await assert.rejects(verifyAsset(windows, macosName, entry, publicKey), /invalid/);
    await assert.rejects(verifyAsset(windows, windowsName, entry, parsePublicKey(other.publicText)), /different key/);
    await assert.rejects(verifyAsset(windows, windowsName, undefined, publicKey), /Missing update signature/);
    const legacy = { ...entry, signature: Buffer.concat([Buffer.from('Ed'), Buffer.from(entry.signature, 'base64').subarray(2)]).toString('base64') };
    await assert.rejects(verifyAsset(windows, windowsName, legacy, publicKey), /prehashed/);
    assert.throws(() => parseSignatureLines(`explorie-signature ${windowsName} only-one-field`), /Malformed/);
    assert.throws(
      () => parseSignatureLines(`explorie-signature a b c\nexplorie-signature a b c`),
      /Duplicate/,
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('the signature block replaces earlier signatures and hides from rendered notes', () => {
  const first = withSignatureBlock('## Changes\n\n- Fix', ['explorie-signature a b c']);
  assert.equal(first, '## Changes\n\n- Fix\n\n<!-- explorie-update-signatures\nexplorie-signature a b c\n-->\n');
  const second = withSignatureBlock(first, ['explorie-signature a d e']);
  assert.equal(second, '## Changes\n\n- Fix\n\n<!-- explorie-update-signatures\nexplorie-signature a d e\n-->\n');
  assert.equal(stripSignatureBlock(second), '## Changes\n\n- Fix');
  assert.equal(withSignatureBlock('', ['explorie-signature a b c']).startsWith('<!-- explorie-update-signatures'), true);
});

test('the signing command enforces key configuration both ways', async () => {
  const { directory, windows, macos } = await fixtureDirectory();
  try {
    const pair = minisignKeyPair('secret');
    const other = minisignKeyPair();
    const configured = path.join(directory, 'configured.pub');
    const unconfigured = path.join(directory, 'unconfigured.pub');
    const notes = path.join(directory, 'notes.md');
    await writeFile(configured, pair.publicText);
    await writeFile(unconfigured, 'untrusted comment: no key\n');
    await writeFile(notes, '## Changes\n');
    const run = (command, publicKey, env = {}) =>
      main([command, '--public-key', publicKey, '--notes', notes, windows, macos], env);

    assert.match(await run('sign', unconfigured), /not configured/);
    assert.equal(await readFile(notes, 'utf8'), '## Changes\n');
    assert.match(await run('verify', unconfigured), /not configured/);
    await assert.rejects(run('sign', configured), /require the UPDATE_SIGNING_KEY secret/);
    await assert.rejects(run('sign', unconfigured, { UPDATE_SIGNING_KEY: pair.secretText }), /has no public key/);
    await assert.rejects(
      run('sign', configured, { UPDATE_SIGNING_KEY: other.secretText }),
      /does not match/,
    );
    await assert.rejects(run('verify', configured), /Missing update signature/);

    assert.match(
      await run('sign', configured, { UPDATE_SIGNING_KEY: pair.secretText, UPDATE_SIGNING_KEY_PASSWORD: 'secret' }),
      /Signed 2 update payload/,
    );
    const signed = await readFile(notes, 'utf8');
    assert.match(signed, /^## Changes\n\n<!-- explorie-update-signatures\nexplorie-signature explorie-1\.2\.3-windows-x64-setup-unsigned\.exe \S+ \S+\nexplorie-signature explorie-1\.2\.3-macos-arm64\.dmg \S+ \S+\n-->\n$/);
    assert.match(await run('verify', configured), /Verified update signatures for 2/);
    await writeFile(macos, randomBytes(1024));
    await assert.rejects(run('verify', configured), /invalid/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('the checked-in update public key is absent or valid', async () => {
  const key = parsePublicKey(await readFile(PUBLIC_KEY_FILE, 'utf8'));
  assert.ok(key === null || key.key.length === 32);
});
