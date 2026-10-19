# Monday download dependency isolation

Upstream: `libduckdb-sys 1.10504.0`, MIT, from crates.io.
Registry archive SHA-256:
`94e67523a40ee3da30411e00e243990b3760d91877202ab2f39c03ab34f8dbfc`.

The native source archive and compiler backends are unchanged.
Bindings retain upstream code; one generated file loses a trailing blank line.
`download` explicitly enables reqwest and zip. System linking and bundled
compilation do not compile these dependencies. Set both the feature and
`DUCKDB_DOWNLOAD_LIB=1` to use the original upstream downloader. Without the
feature, that environment variable fails with an actionable error.
Bundled mode retains precedence over the download environment variable.

The research-core, data-pipelines, and prediction-markets manifests select this patch.
The optional downloader uses zip 9.0.5 or later, including the extraction fixes
for RUSTSEC-2026-0367, RUSTSEC-2026-0368, and RUSTSEC-2026-0369.
The native cache identity binds every patch file, features, dependency checksums,
compiler, ABI, flags and profiles. Only this exact native path is retained during
cache cleanup. Application path dependencies are still removed.
Fresh CI checkouts normalize committed native source timestamps before cache
lookup. Complete source hashes isolate changed bytes. Preparation rejects dirty
source and existing owning targets. Local builds retain actual timestamps.
