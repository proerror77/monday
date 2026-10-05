# CEX Campaign prepared development inputs

The V6 plain native Campaign freezes `CampaignPreparedInputsV1` before its
augmented native inputs receipt and finalized request. The collection has three
actual `Validation` views: label-free Features, actual future mid-price marks,
and canonical Replay events. It never exposes a generic Train/Training exit.

The trusted control exporter verifies the original materialization, source
receipts, full row schedule, PIT clocks, purge and embargo. Every development
label is reconstructed from the actual future observation at its frozen clock.
A future mark or maturity in selection or holdout rejects export. No rows or
prices are invented, interpolated, silently cropped, or relabeled as Train.

The current fixed Calendar contract starts selection at `develop_end`, without
an authorized future-price gap. Its development tail therefore requires marks
inside selection. Export rejects that Calendar plan until an explicit learning
row and observation-tail contract is admitted. Ordinary non-Calendar V6 inputs
do not inherit that limitation. A final evaluation grant alone cannot restore
withheld read capabilities removed from a development request.

`hft_cex_research_input::campaign::VerifiedCampaignPreparedInputsV1` is the opaque
readback result. Its collection identity binds original total/partition metadata,
protocol, label recipe, native ResearchRow hash, source Build, native preparation
Run and receipt, and actual view/block identities. `expected_native()` verifies
data equivalence; it grants no budget, lease, Run, or final evaluation authority.

The finalized request exposes only the collection and exact content-addressed
allowed blocks. Operator finalize reads them back before producing submission.
The scientific worker decodes the same collection, then passes genuine
development rows into the original GP kernel and fitting code. Shared Replay
uses the original Rust accounting kernel with an endpoint inside the authorized
context. Its receipt binds the actual typed view, source, decisions and trace.

The engine preserves the original full-data fold and partition schedule while
storing only actual development rows. Missing selection and holdout bytes hard
reject their evaluators. Calendar development results contain no independent
selection receipt and cannot select a candidate through that gate. Independently
read-back native result ZIPs must retain the same request, collection, source,
protocol and trial bindings. A development result remains insufficient evidence
for a full scientific conclusion or budget refund.

Each native result ZIP stores the actual descriptor and its allowed encoded
blocks under `results/native-prepared-blocks/<sha256>.mondaybin`. Independent
recovery requires the finalized native request and its independently bound
SHA256. It decodes those archived blocks through the same opaque input inspector,
rebuilds the metadata-only dataset, and reuses the original model ledger and
replay validators. The original feature, materialization, and dataset manifests
remain exact lineage metadata. Development bytes never use the original
full-source feature SHA filename. Recovery rejects altered or missing blocks,
foreign lineage metadata, and withheld evaluation artifacts.

The platform worker verifies the admitted Attempt, exact static configuration,
native signature, source revision, protocol and expiry before fitting. Static
files use `/config`; per-Attempt credentials use `/identity`. Credentials never
enter the static configuration hash. The private reader hashes and parses one
snapshot of the static files. No signing key or database credential reaches the
worker.

After independent native ZIP recovery, the worker uploads the actual campaign
result and round archives through the scoped TLS writer. Immutable PUT recovery
requires actual GET bytes. `cex-campaign.json` binds the original request,
collection, source, trials and decoded archive entries. `receipt.json` binds the
same artifacts to the admitted Task, Attempt and fence. Successful transport
keeps the scientific status `insufficient_evidence`.

Platform CexCampaign admission, its transferred native budget, the platform
lease/attempt fence and terminal settlement are separate enforced boundaries.
The data inspector is not a substitute for any of them. Sequence and market
encoder Campaign schemas retain their separate existing input contracts.
