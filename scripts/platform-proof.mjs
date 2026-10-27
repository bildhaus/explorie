import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';

const proofPath = path.join(process.cwd(), '.release-checks', 'platform-proof.json');
const required = {
  windows: [
    'installerInstalled',
    'installerCleanupOfferedAndCompleted',
    'automaticUpdateReplacedCleanedAndReopened',
    'headedConsoleAbsent',
    'multiWindowIsolation',
    'inboundDragDrop',
    'outboundDragDrop',
    'mixedDpiWindowRestore',
    'crashRestore',
    'folderIntegrationEnabled',
    'folderIntegrationDisabled',
    'folderIntegrationRestoredAfterUninstall',
    'disposableFilesystemOperations',
    'remoteDriveLifecycle',
    'bundledIntegrationsActivated',
  ],
  macos: [
    'dmgInstalledAndLaunched',
    'dmgCleanupOfferedAndCompleted',
    'automaticUpdateReplacedCleanedAndReopened',
    'signatureValid',
    'notarizationValid',
    'gatekeeperAccepted',
    'multiWindowIsolation',
    'inboundDragDrop',
    'outboundDragDrop',
    'multiMonitorWindowRestore',
    'crashRestore',
    'folderIntegrationEnabled',
    'folderIntegrationDisabled',
    'disposableFilesystemOperations',
    'remoteDriveLifecycle',
    'bundledIntegrationsActivated',
  ],
};

async function packageMetadata() {
  return JSON.parse(await readFile(path.join(process.cwd(), 'package.json'), 'utf8'));
}

async function initialize() {
  const checks = (platform) => Object.fromEntries(required[platform].map((name) => [name, false]));
  const proof = {
    version: (await packageMetadata()).version,
    candidateTag: '',
    windows: { artifact: '', sha256: '', machine: '', testedAt: '', checks: checks('windows') },
    macos: { artifact: '', sha256: '', machine: '', testedAt: '', checks: checks('macos') },
    notes: '',
  };
  await mkdir(path.dirname(proofPath), { recursive: true });
  await writeFile(proofPath, `${JSON.stringify(proof, null, 2)}\n`, { flag: 'wx' });
  console.log(proofPath);
}

function validatePlatform(proof, platform, version, firstRelease, errors) {
  const candidate = proof[platform];
  if (!candidate || typeof candidate !== 'object') {
    errors.push(`${platform}: proof section is missing`);
    return;
  }
  for (const field of ['artifact', 'machine', 'testedAt']) {
    if (typeof candidate[field] !== 'string' || candidate[field].trim() === '') {
      errors.push(`${platform}: ${field} is required`);
    }
  }
  const expectedArtifact =
    platform === 'windows'
      ? `explorie-${version}-windows-x64-setup-unsigned.exe`
      : `explorie-${version}-macos-arm64.dmg`;
  if (candidate.artifact !== expectedArtifact) {
    errors.push(`${platform}: artifact must be ${expectedArtifact}`);
  }
  if (!/^[a-f0-9]{64}$/i.test(candidate.sha256 ?? '')) {
    errors.push(`${platform}: sha256 must be the tested artifact's 64-character digest`);
  }
  // macOS 26 keeps Finder as the folder handler and refuses every change to
  // it, so the folder-integration checks may be marked not applicable there,
  // with the reason recorded and verified in Settings > System Integration.
  const folderIntegrationUnavailable =
    platform === 'macos' &&
    typeof candidate.folderIntegrationUnavailable === 'string' &&
    candidate.folderIntegrationUnavailable.trim() !== '';
  for (const check of required[platform]) {
    if (
      firstRelease &&
      check === 'automaticUpdateReplacedCleanedAndReopened' &&
      candidate.checks?.[check] === 'not-applicable'
    ) {
      continue;
    }
    if (
      folderIntegrationUnavailable &&
      (check === 'folderIntegrationEnabled' || check === 'folderIntegrationDisabled') &&
      candidate.checks?.[check] === 'not-applicable'
    ) {
      continue;
    }
    if (candidate.checks?.[check] !== true) {
      errors.push(`${platform}: ${check} was not attested`);
    }
  }
}

async function verify() {
  const proof = JSON.parse(await readFile(proofPath, 'utf8'));
  const { version, repository } = await packageMetadata();
  const errors = [];
  const firstRelease = Object.hasOwn(proof, 'firstRelease');
  if (firstRelease) {
    const repositoryUrl = typeof repository === 'string' ? repository : repository?.url;
    if (
      version !== '0.0.1' ||
      typeof repositoryUrl !== 'string' ||
      !/^https:\/\/github\.com\/bildhaus\/explorie(?:\.git)?\/?$/i.test(repositoryUrl)
    ) {
      errors.push('firstRelease: updater exemption is only valid for bildhaus/explorie v0.0.1');
    }
    if (typeof proof.firstRelease?.reason !== 'string' || !proof.firstRelease.reason.trim()) {
      errors.push('firstRelease: a reason for the updater exemption is required');
    }
  }
  if (proof.version !== version) errors.push(`version: expected ${version}, got ${proof.version}`);
  if (proof.candidateTag !== `v${version}`) {
    errors.push(`candidateTag: expected v${version}, got ${proof.candidateTag || '(empty)'}`);
  }
  validatePlatform(proof, 'windows', version, firstRelease, errors);
  validatePlatform(proof, 'macos', version, firstRelease, errors);
  if (errors.length > 0) {
    throw new Error(`Platform proof is incomplete:\n- ${errors.join('\n- ')}`);
  }
  console.log(`Platform proof is complete for v${version}.`);
  if (firstRelease) console.log(`First-release updater exemption: ${proof.firstRelease.reason}`);
  console.log(`Windows tested SHA-256: ${proof.windows.sha256.toLowerCase()}`);
  console.log(`macOS tested SHA-256: ${proof.macos.sha256.toLowerCase()}`);
}

const command = process.argv[2];
if (command === 'init') {
  await initialize();
} else if (command === 'verify') {
  await verify();
} else {
  throw new Error('Usage: node scripts/platform-proof.mjs <init|verify>');
}
