export function queryViewState(data, error) {
  if (error) return { kind: data == null ? 'error' : 'stale', data, message: error instanceof Error ? error.message : String(error) };
  if (data == null) return { kind: 'loading', data: null, message: null };
  return { kind: 'success', data, message: null };
}

export function mutationError(error) {
  return error ? (error instanceof Error ? error.message : String(error)) : null;
}

export function batchFailure(results) {
  const failures = results.filter((result) => result.status === 'rejected');
  return failures.length === 0 ? null : `${failures.length} action(s) failed: ${failures.map((result) => mutationError(result.reason)).join('; ')}`;
}
