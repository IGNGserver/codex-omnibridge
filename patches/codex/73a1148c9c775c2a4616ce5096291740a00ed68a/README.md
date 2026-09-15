# Codex Core per-turn local Router patch

This patch is pinned to upstream Codex commit:

```text
73a1148c9c775c2a4616ce5096291740a00ed68a
```

It is the P0 implementation required to route a namespaced logical model from the real Codex Core data path without changing `model_provider`, Thread provider, ChatGPT OAuth, or `auth.json`.

## Files changed

- `core/src/logical_routing.rs`
  - Defines official vs. local-router logical routing.
  - Loads and validates `CODEX_MP_ROUTER_ENDPOINT_FILE`.
  - Requires a loopback HTTP endpoint and a private endpoint file on Unix.
- `core/src/client.rs`
  - Captures the endpoint once per `ModelClient` session.
  - Branches inside `ModelClientSession::stream()`.
  - Sends custom turns to the Router with the raw `x-codex-omnibridge-token` capability header.
  - Keeps official turns on the existing OpenAI/ChatGPT transport.
  - Skips custom WebSocket prewarm and rejects custom compaction.
  - Does not add ChatGPT OAuth to the custom Router request.
- `core/src/lib.rs`
  - Registers the new Core module.

## Apply

`apply.sh` accepts either the `codex-rs` checkout or its parent directory:

```bash
./apply.sh /path/to/codex-rs
# or
./apply.sh /path/to/parent-containing/codex-rs
```

The script verifies the exact upstream HEAD, runs `git apply --check`, and refuses to reset, clean, overwrite unrelated work, or create a commit. If the patch is already applied it exits successfully without applying it a second time.

## Build evidence

The audited source was built with:

```bash
CARGO_TARGET_DIR=/home/lvziw/codex-rs-target \
OPENSSL_DIR=/usr \
OPENSSL_LIB_DIR=/usr/lib/x86_64-linux-gnu \
OPENSSL_INCLUDE_DIR=/usr/include \
cargo check -p codex-core

CARGO_TARGET_DIR=/home/lvziw/codex-rs-target \
OPENSSL_DIR=/usr \
OPENSSL_LIB_DIR=/usr/lib/x86_64-linux-gnu \
OPENSSL_INCLUDE_DIR=/usr/include \
cargo test -p codex-core logical_routing
```

The targeted Core test covers official/custom route selection, valid loopback endpoint loading, non-loopback rejection, and Unix endpoint permissions. The patch itself does not build or package Codex Desktop.

## Runtime setup

Start the MultiProvider Router and pass its endpoint file to the **same patched Codex process**:

```bash
ENDPOINT_FILE="$HOME/.config/codexmultiprovider/router-endpoint.json"
codex-mp router --port 0 --endpoint-file "$ENDPOINT_FILE"
export CODEX_MP_ROUTER_ENDPOINT_FILE="$ENDPOINT_FILE"
/path/to/patched/codex app-server
```

The endpoint file contains the capability token and is written as mode 0600 on Unix. Do not commit, copy, log, or expose it. A pinned Linux `codex-cli` and `codex-app-server` release build has completed an isolated custom turn and an Official → Custom → Official app-server thread using controlled official/custom mocks. A real ChatGPT Account, real third-party Provider, Desktop picker, and native Windows/macOS patched artifacts are still required for product-level E2E proof.
