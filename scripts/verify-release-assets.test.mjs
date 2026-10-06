import assert from 'node:assert/strict';
import { createHash, generateKeyPairSync, randomBytes } from 'node:crypto';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { signAsset, withSignatureBlock } from './update-signatures.mjs';
import { verifyReleaseAssets } from './verify-release-assets.mjs';

async function fixture(t) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'explorie-release-assets-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const repository = 'bildhaus/explorie', tag = 'v0.0.1';
  const release = { tag_name: tag, draft: true, prerelease: false, assets: [] };
  const attestations = {};
  for (const [platform, suffix] of Object.entries({ windows: 'windows-x64-setup-unsigned.exe', macos: 'macos-arm64.dmg' })) {
    const name = `explorie-0.0.1-${suffix}`;
    const bytes = Buffer.from(`${platform} verified installer`);
    const hash = createHash('sha256').update(bytes).digest('hex');
    attestations[platform] = hash;
    await writeFile(path.join(directory, name), bytes);
    release.assets.push({ name, state: 'uploaded', size: bytes.length, digest: `sha256:${hash}`,
      browser_download_url: `https://github.com/${repository}/releases/download/${tag}/${name}` });
  }
  return { release, directory, repository, tag, attestations };
}

test('verifies exactly two installers against GitHub digests and human attestations', async t => {
  const f = await fixture(t);
  await verifyReleaseAssets(f.release, f.directory, f.repository, f.tag);
  await verifyReleaseAssets(f.release, f.directory, f.repository, f.tag, f.attestations);
});

test('rejects missing, duplicate, extra and foreign platform assets', async t => {
  const f = await fixture(t);
  for (const assets of [[], [f.release.assets[0]], [...f.release.assets, { name: 'SHA256SUMS.txt' }],
    [f.release.assets[0], f.release.assets[0]], [f.release.assets[0], { ...f.release.assets[1], name: 'portable.zip' }]]) {
    await assert.rejects(verifyReleaseAssets({ ...f.release, assets }, f.directory, f.repository, f.tag));
  }
});

test('rejects wrong release identity or a release already published', async t => {
  const f = await fixture(t);
  for (const patch of [{ tag_name: 'v0.1.1' }, { draft: false }, { prerelease: true }]) {
    await assert.rejects(verifyReleaseAssets({ ...f.release, ...patch }, f.directory, f.repository, f.tag), /exact draft/);
  }
});

test('accepts the untagged download URLs GitHub gives draft assets', async t => {
  const f = await fixture(t);
  const assets = f.release.assets.map(asset => ({ ...asset,
    browser_download_url: `https://github.com/${f.repository}/releases/download/untagged-13c4fc92a6560bd999a6/${asset.name}` }));
  await verifyReleaseAssets({ ...f.release, assets }, f.directory, f.repository, f.tag);
});

test('fails closed on malformed digest, wrong URL, size or upload state', async t => {
  const f = await fixture(t);
  for (const patch of [{ digest: null }, { digest: '' }, { digest: `sha512:${'a'.repeat(64)}` },
    { digest: `sha256:${'g'.repeat(64)}` }, { digest: 'sha256:abc' }, { state: 'starter' },
    { size: 0 }, { size: 0.5 }, { size: f.release.assets[0].size + 1 },
    { browser_download_url: f.release.assets[0].browser_download_url.replace('github.com', 'example.com') },
    { browser_download_url: f.release.assets[0].browser_download_url.replace('bildhaus/explorie', 'bildhaus/other') },
    { browser_download_url: `https://github.com/${f.repository}/releases/download/untagged-XYZ/${f.release.assets[0].name}` },
    { browser_download_url: `https://github.com/${f.repository}/releases/download/untagged-abc/nested/${f.release.assets[0].name}` },
    { browser_download_url: `https://github.com/${f.repository}/releases/download/untagged-abc/other.exe` },
    { browser_download_url: `https://github.com/${f.repository}/releases/download/v9.9.9/${f.release.assets[0].name}` }]) {
    const release = { ...f.release, assets: [{ ...f.release.assets[0], ...patch }, f.release.assets[1]] };
    await assert.rejects(verifyReleaseAssets(release, f.directory, f.repository, f.tag));
  }
});

test('rejects changed bytes even when installer length is unchanged', async t => {
  const f = await fixture(t);
  await writeFile(path.join(f.directory, f.release.assets[0].name), Buffer.alloc(f.release.assets[0].size));
  await assert.rejects(verifyReleaseAssets(f.release, f.directory, f.repository, f.tag), /SHA-256 mismatch/);
});

test('publication requires both exact tested hashes', async t => {
  const f = await fixture(t);
  for (const proof of [{}, { windows: f.attestations.windows },
    { ...f.attestations, macos: 'b'.repeat(64) }, { ...f.attestations, windows: 'bad' }]) {
    await assert.rejects(verifyReleaseAssets(f.release, f.directory, f.repository, f.tag, proof), /real-machine-tested/);
  }
});

test('a configured update key requires valid signatures for both installers', async t => {
  const f = await fixture(t);
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const keyId = randomBytes(8);
  const key = publicKey.export({ format: 'der', type: 'spki' }).subarray(12);
  const secret = { keyId, key, privateKey };
  const lines = [];
  for (const asset of f.release.assets) {
    lines.push(await signAsset(path.join(f.directory, asset.name), asset.name, secret));
  }
  const signed = { ...f.release, body: withSignatureBlock('Notes', lines) };
  await verifyReleaseAssets(signed, f.directory, f.repository, f.tag, f.attestations, { keyId, key });
  // Without a configured key the notes are not consulted.
  await verifyReleaseAssets({ ...f.release, body: 'Notes' }, f.directory, f.repository, f.tag);
  for (const body of ['Notes', withSignatureBlock('Notes', lines.slice(0, 1)),
    withSignatureBlock('Notes', [lines[0], lines[1].replace(f.release.assets[1].name, f.release.assets[0].name)])]) {
    await assert.rejects(
      verifyReleaseAssets({ ...f.release, body }, f.directory, f.repository, f.tag, undefined, { keyId, key }),
    );
  }
});
