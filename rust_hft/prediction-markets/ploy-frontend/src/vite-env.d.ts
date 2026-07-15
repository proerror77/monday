/// <reference types="vite/client" />

declare module '@/lib/operatorViewState.mjs' {
  export type QueryViewState<T> = { kind: 'loading' | 'success' | 'error' | 'stale'; data: T | null | undefined; message: string | null };
  export function queryViewState<T>(data: T | null | undefined, error: unknown): QueryViewState<T>;
  export function mutationError(error: unknown): string | null;
  export function batchFailure(results: PromiseSettledResult<unknown>[]): string | null;
}
