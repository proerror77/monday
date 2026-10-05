import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {mkdtempSync, writeFileSync, rmSync, mkdirSync, readFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {fileURLToPath} from 'node:url';
const script = fileURLToPath(new URL('./local-lock-impact.sh', import.meta.url));
function localLockImpact(before, after) {
  const dir = mkdtempSync(`${tmpdir()}/lock-impact-`);
  try {
    writeFileSync(`${dir}/before`, before); writeFileSync(`${dir}/after`, after);
    return JSON.parse(execFileSync('bash', [script, '--files', `${dir}/before`, `${dir}/after`], {encoding:'utf8',stdio:['ignore','pipe','pipe']}));
  } finally { rmSync(dir, {recursive:true,force:true}); }
}

const local = `[[package]]
name = "manifest"
version = "0.1.0"
dependencies = [
 "bytes",
]

`;
const external = `[[package]]
name = "bytes"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "abc"

`;
const before = '# Generated\nversion = 4\n\n' + local + external;
assert.deepEqual(localLockImpact(before, before), []);
assert.deepEqual(localLockImpact(before, before.replace(' "bytes",', ' "bytes",\n "parquet",')), ['manifest']);
assert.deepEqual(localLockImpact(before, before.replace('dependencies = [\n "bytes",\n]\n', '')), ['manifest']);
for (const after of [
  before.replace('checksum = "abc"', 'checksum = "def"'),
  before.replace('version = "1.0.0"', 'version = "2.0.0"'),
  before.replace('version = "0.1.0"', 'version = "0.2.0"'),
  before + external,
  before.replace(external, ''),
  before.replace('version = 4', 'version = 5'),
  before.replace(' "bytes",', ' unknown = true,'),
  before.replace('checksum = "abc"', 'checksum = "abc"\ndependencies = [\n "manifest",\n]'),
]) assert.throws(() => localLockImpact(before, after));
console.log('local lock impact: dependency-only edits narrow; external/unknown edits broaden');

// Exercise the actual selector across a lock-only Git diff and downstream graph.
const repo = mkdtempSync(`${tmpdir()}/lock-scope-`);
try {
  const git = (...args) => execFileSync('git', ['-C', repo, ...args], {encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();
  git('init'); git('config','user.name','CI fixture'); git('config','user.email','ci@example.invalid');
  git('config','core.hooksPath','/dev/null');
  mkdirSync(`${repo}/rust_hft`);
  const lock = before.replace('name = "manifest"','name = "hft-core"');
  writeFileSync(`${repo}/rust_hft/Cargo.lock`, lock);
  git('add','.'); git('commit','-m','fixture base'); const base = git('rev-parse','HEAD');
  writeFileSync(`${repo}/rust_hft/Cargo.lock`, lock.replace(' "bytes",',' "bytes",\n "existing-dep",'));
  git('commit','-am','local edge'); const head = git('rev-parse','HEAD');
  const selector = fileURLToPath(new URL('./select-rust-ci-scope.sh',import.meta.url));
  const metadata = fileURLToPath(new URL('./fixtures/rust-ci-scope/metadata.fixture',import.meta.url));
  for (const event of ['pull_request','push']) {
    writeFileSync(`${repo}/scope.out`, '');
    execFileSync('bash',[selector,'--base',base,'--head',head,'--event',event,'--metadata',metadata,'--output',`${repo}/scope.out`],{cwd:repo,encoding:'utf8'});
    const out = readFileSync(`${repo}/scope.out`, 'utf8');
    assert.match(out,/ci\/rust-hft-engine-fast-lane/);
    assert.match(out,/owning_packages=,hft-core,/);
    assert.match(out,/collector=false/); assert.match(out,/handoff=true/);
    assert.match(out,/focused_packages=,hft-live,/);
    assert.doesNotMatch(out,/research-image-binaries|rust-research-heavy/);
  }
  writeFileSync(`${repo}/rust_hft/Cargo.lock`, lock.replace('checksum = "abc"','checksum = "def"'));
  git('commit','-am','external package');
  writeFileSync(`${repo}/scope.out`, '');
  execFileSync('bash',[selector,'--base',base,'--head',git('rev-parse','HEAD'),'--event','pull_request','--metadata',metadata,'--output',`${repo}/scope.out`],{cwd:repo,encoding:'utf8',stdio:['ignore','pipe','pipe']});
  const out = readFileSync(`${repo}/scope.out`, 'utf8');
  assert.match(out,/collector=true/); assert.match(out,/research-image-binaries/);
  for (const [workspace,name,broad] of [
    ['rust_hft','rust-hft-workspace',true],
    ['rust_hft/prediction-markets','ploy',true],
    ['rust_hft/prediction-markets','hft-core',false],
    ['rust_hft/prediction-markets','unknown-owner',true],
  ]) {
    mkdirSync(`${repo}/${workspace}`,{recursive:true});
    const path = `${repo}/${workspace}/Cargo.lock`;
    const value = before.replace('name = "manifest"',`name = "${name}"`);
    writeFileSync(path,value); git('add',`${workspace}/Cargo.lock`); git('commit','-m','scenario base');
    const scenarioBase=git('rev-parse','HEAD');
    writeFileSync(path,value.replace(' "bytes",',' "bytes",\n "existing-dep",'));
    git('add',`${workspace}/Cargo.lock`); git('commit','-m','scenario head');
    writeFileSync(`${repo}/scope.out`,'');
    execFileSync('bash',[selector,'--base',scenarioBase,'--head',git('rev-parse','HEAD'),'--event','pull_request','--metadata',metadata,'--output',`${repo}/scope.out`],{cwd:repo,encoding:'utf8'});
    const out=readFileSync(`${repo}/scope.out`,'utf8');
    if(broad) assert.match(out,/research-image-binaries/);
    else { assert.match(out,/ci\/rust-hft-engine-fast-lane/); assert.match(out,/owning_packages=,hft-core,/); }
  }
} finally { rmSync(repo,{recursive:true,force:true}); }
console.log('lock-only Git diff: owning package + reverse dependents; PR/push verified');
