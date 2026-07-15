import fs from 'node:fs';
import assert from 'node:assert/strict';
import { batchFailure, mutationError, queryViewState } from '../src/lib/operatorViewState.mjs';

const api = fs.readFileSync(new URL('../src/services/api.ts', import.meta.url), 'utf8');
const app = fs.readFileSync(new URL('../src/App.tsx', import.meta.url), 'utf8');
for (const retired of ['/stats/', "'/trades", "'/positions", "'/security/events", "'/system/start", "'/config", "'/agent/", "'/sidecar/"]) assert.equal(api.includes(retired), false, `retired route: ${retired}`);
assert.match(api, /endpoint\.startsWith\('\/auth\/'\)/);
assert.match(app, /ws\.connect\(\)/);
assert.match(app, /ws\.disconnect\(\)/);
assert.equal(queryViewState(undefined, new Error('offline')).kind, 'error');
assert.equal(queryViewState([{ id: 1 }], new Error('stale')).kind, 'stale');
assert.equal(queryViewState([], null).kind, 'success');
assert.equal(mutationError(new Error('denied')), 'denied');
assert.match(batchFailure([{ status: 'fulfilled', value: null }, { status: 'rejected', reason: new Error('one failed') }]), /one failed/);
console.log('canonical route and visible error view-model contract: ok');
