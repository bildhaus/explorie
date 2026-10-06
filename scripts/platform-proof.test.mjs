import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const script = fileURLToPath(new URL('./platform-proof.mjs', import.meta.url));
const updateCheck = 'automaticUpdateReplacedCleanedAndReopened';

async function fixture(t, version = '0.0.1', repository = 'https://github.com/bildhaus/explorie') {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'explorie-platform-proof-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  await writeFile(path.join(directory, 'package.json'), JSON.stringify({ version, repository }));
  const run = (command) => spawnSync(process.execPath, [script, command], {
    cwd: directory,
    encoding: 'utf8',
  });
  const initialized = run('init');
  assert.equal(initialized.status, 0, initialized.stderr);
  const proofPath = path.join(directory, '.release-checks', 'platform-proof.json');
  const proof = JSON.parse(await readFile(proofPath, 'utf8'));
  proof.candidateTag = `v${version}`;
  for (const platform of ['windows', 'macos']) {
    Object.assign(proof[platform], {
      artifact: platform === 'windows'
        ? `explorie-${version}-windows-x64-setup-unsigned.exe`
        : `explorie-${version}-macos-arm64.dmg`,
      sha256: 'a'.repeat(64),
      machine: `Physical ${platform} test machine`,
      testedAt: '2026-09-09T09:00:00Z',
    });
    for (const check of Object.keys(proof[platform].checks)) proof[platform].checks[check] = true;
  }
  return {
    proof,
    verify: async () => {
      await writeFile(proofPath, JSON.stringify(proof));
      return run('verify');
    },
    run,
  };
}

function exempt(proof) {
  proof.firstRelease = { reason: 'First public Bildhaus release; there is no previous Bildhaus version.' };
  for (const platform of ['windows', 'macos']) proof[platform].checks[updateCheck] = 'not-applicable';
}

test('normal releases accept completed proof without a first-release field', async (t) => {
  const { proof, verify } = await fixture(t, '0.3.2', 'https://github.com/bildhaus/explorie');
  assert.equal(Object.hasOwn(proof, 'firstRelease'), false);
  const result = await verify();
  assert.equal(result.status, 0, result.stderr);
});

test('initialization never overwrites existing attestations', async (t) => {
  const { verify, run } = await fixture(t);
  assert.equal((await verify()).status, 0);
  assert.notEqual(run('init').status, 0);
  assert.equal(run('verify').status, 0);
});

test('Bildhaus v0.0.1 allows explicit updater N/A with a reason', async (t) => {
  const { proof, verify } = await fixture(t);
  exempt(proof);
  const result = await verify();
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /First-release updater exemption:/);
});

test('first-release exemption accepts the equivalent GitHub repository metadata object', async (t) => {
  const { proof, verify } = await fixture(t, '0.0.1', { type: 'git', url: 'https://github.com/Bildhaus/explorie.git' });
  exempt(proof);
  const result = await verify();
  assert.equal(result.status, 0, result.stderr);
});

test('updater N/A is rejected without the explicit first-release declaration', async (t) => {
  const { proof, verify } = await fixture(t);
  for (const platform of ['windows', 'macos']) proof[platform].checks[updateCheck] = 'not-applicable';
  const result = await verify();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /windows: automaticUpdateReplacedCleanedAndReopened was not attested/);
  assert.match(result.stderr, /macos: automaticUpdateReplacedCleanedAndReopened was not attested/);
});

test('first-release mode requires a nonempty reason', async (t) => {
  const { proof, verify } = await fixture(t);
  exempt(proof);
  for (const declaration of [null, {}, { reason: '' }, { reason: '  ' }, { reason: true }]) {
    proof.firstRelease = declaration;
    const result = await verify();
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /firstRelease: a reason/);
  }
});

test('later versions and unrelated repositories cannot bypass updater proof', async (t) => {
  for (const [version, repository] of [
    ['0.0.2', 'https://github.com/bildhaus/explorie'],
    ['0.1.0', 'https://github.com/bildhaus/explorie'],
    ['0.3.2', 'https://github.com/bildhaus/explorie'],
    ['0.0.1-beta.1', 'https://github.com/bildhaus/explorie'],
    ['0.0.1', 'https://github.com/oshtz/explorie'],
    ['0.0.1', 'https://github.com/bildhaus/other'],
    ['0.0.1', 'https://github.com/bildhaus/explorie/extra'],
    ['0.0.1', 'https://github.com.evil.example/bildhaus/explorie'],
    ['0.0.1', null],
  ]) {
    const { proof, verify } = await fixture(t, version, repository);
    exempt(proof);
    const result = await verify();
    assert.notEqual(result.status, 0, `${version} ${repository}`);
    assert.match(result.stderr, /only valid for bildhaus\/explorie v0.0.1/);
  }
});

test('first-release mode requires explicit N/A and never waives other checks', async (t) => {
  const { proof, verify } = await fixture(t);
  exempt(proof);
  for (const platform of ['windows', 'macos']) {
    assert.equal(proof[platform].checks.bundledIntegrationsActivated, true);
    assert.equal(proof[platform].checks.remoteDriveLifecycle, true);
    for (const check of Object.keys(proof[platform].checks)) {
      const original = proof[platform].checks[check];
      proof[platform].checks[check] = false;
      const result = await verify();
      assert.notEqual(result.status, 0, `${platform} ${check}`);
      assert.ok(result.stderr.includes(`${platform}: ${check} was not attested`), result.stderr);
      proof[platform].checks[check] = original;
    }
  }
});

test('first-release mode still rejects stale version, tag, artifact, and digest', async (t) => {
  const { proof, verify } = await fixture(t);
  exempt(proof);
  proof.version = '0.3.2';
  proof.candidateTag = 'v0.3.2';
  proof.windows.artifact = 'explorie-0.3.2-windows-x64-setup-unsigned.exe';
  proof.macos.sha256 = 'invalid';
  const result = await verify();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /version: expected 0.0.1/);
  assert.match(result.stderr, /candidateTag: expected v0.0.1/);
  assert.match(result.stderr, /windows: artifact must be explorie-0.0.1/);
  assert.match(result.stderr, /macos: sha256 must be/);
});
