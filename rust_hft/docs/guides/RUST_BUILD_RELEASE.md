# Rust 構建與發布指南

這個 workspace 把構建路徑分成兩條：

- 開發與 CI 檢查：編譯要快，盡量復用快取。
- Release 發布：二進制要小、吞吐要高、產物資訊要可追溯。

## 開發路徑

dev profile 保持 Cargo 增量編譯，並禁用 LTO。不要在 `.cargo/config.toml` 里用 `CARGO_INCREMENTAL=1` 硬設環境變量；`sccache` 會拒絕這種組合。增量策略交給 Cargo profile 即可。

```toml
[profile.dev]
incremental = true
lto = false
```

倉庫級 Cargo 配置會把 `sccache` 設成 Rust 編譯器 wrapper。本地構建前先安裝：

```bash
cargo install sccache --locked
sccache --start-server
```

Linux 構建透過 `.cargo/config.toml` 使用 `clang` + `mold`，所以 Linux 開發機和 CI host 都需要安裝：

```bash
sudo apt-get update
sudo apt-get install -y clang mold
```

如果本地 sccache 暫時不可用，不要直接改共享配置，先對單次命令覆蓋：

```bash
cargo --config 'build.rustc-wrapper=""' check
```

## CI 快取路徑

CI 顯式設置：

```bash
RUSTC_WRAPPER=sccache
SCCACHE_GHA_ENABLED=true
CARGO_INCREMENTAL=0
TZ=UTC LC_ALL=C LANG=C
```

`CARGO_INCREMENTAL=0` 是有意為之：Rust 的增量編譯產物不能被 `sccache` 有效復用。也就是說，本地開發保留增量編譯，CI 則關閉增量編譯，讓 sccache 共享 crate 級編譯快取。

CI cache key 至少要按下列維度隔離：

- Rust toolchain
- runner OS / target triple
- feature set
- sccache-enabled 與非 sccache job
- sccache version
- `Cargo.lock` 變更，這部分由 `Swatinem/rust-cache` 處理

不要讓不可信 PR 和受保護 release job 共用可寫快取。PR job 只 restore cache，不 save cache；release job 從源碼重建，並發布 checksum。

這個 monorepo 的實際 GitHub workflow 在倉庫根目錄 `.github/workflows/`。`rust_hft/.github/workflows/` 內的文件只在 `rust_hft` 被拆成獨立倉庫時才會被 GitHub 直接觸發。

## Release Profile

release profile 優先保持運行時性能，同時縮小可分發產物：

```toml
[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
panic = "abort"
strip = "symbols"
split-debuginfo = "packed"
```

只有在基準測試證明 latency 或 throughput 有明確收益時，才把 `lto` 改成 `"fat"`，因為 fat LTO 會顯著拉長 link 時間。

## 可復現發布構建

發布自動化中固定時間戳與 locale：

```bash
export SOURCE_DATE_EPOCH="$(git log -1 --format=%ct)"
export TZ=UTC LC_ALL=C LANG=C
cargo build --release -p hft-live
```

每個發布產物旁邊都生成 checksum：

```bash
shasum -a 256 target/release/hft-live > hft-live.sha256
```

## 可攜 Linux 二進制

客戶環境或部署環境的系統庫不可控時，優先用獨立的 static target，並先驗證 C library 依賴：

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl -p hft-live
```

依賴 OpenSSL、zlib 或其他 C library 的 crate，需要對應 `*-sys` 依賴支持靜態構建。能用純 Rust TLS stack 時，優先用 rustls。

根目錄 release workflow 會同時產出 `x86_64-unknown-linux-gnu` 和 `x86_64-unknown-linux-musl`。musl job 在 CI 內安裝 `musl-tools`，並只在該 target 上設置 `CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc`，避免破壞 macOS 本地構建。

## 容器發布形態

用 multi-stage build，runtime image 只保留運行所需內容。需要出站 HTTPS 的二進制，不要盲目用 `scratch`；用 distroless/static，或把 CA certificates 複製進最終 image。

```dockerfile
FROM rust:1.91 AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p hft-live

FROM gcr.io/distroless/cc-debian12
COPY --from=builder /app/target/release/hft-live /usr/local/bin/hft-live
USER 10001
ENTRYPOINT ["/usr/local/bin/hft-live"]
```

## Release Workflow

倉庫根目錄 `.github/workflows/release-rust.yml` 會在 `v*` tag 或手動 dispatch 時發布核心二進制：

- `hft-live`
- `hft-paper`
- `hft-all-in-one`
- `hft-replay`
- `hft-backtest`
- `hft-collector`

每個平台產出一個 `tar.gz` 和一個 `.sha256`。發布 job 使用 `--locked`，所以 `Cargo.lock` 必須納入版本控制；這是 release 可復現的基本邊界。
