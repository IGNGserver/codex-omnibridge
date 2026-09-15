# Third-party notices

## CC Switch

This project contains protocol conversion algorithms and test fixtures adapted
from [CC Switch](https://github.com/farion1231/cc-switch), commit
`1d5d90f4aba88447d422a16cdec5282ec5331fd7` (v3.20.3), under the MIT License.

The preserved source provenance is under
`third_party/cc-switch/1d5d90f4aba88447d422a16cdec5282ec5331fd7/`:

- `proxy/providers/transform_codex_chat.rs`
- `proxy/providers/streaming_codex_chat.rs`
- `proxy/providers/codex_responses_sse.rs`
- `proxy/providers/codex_chat_common.rs`
- `proxy/providers/codex_chat_history.rs`

The local protocol bridge keeps the upstream algorithm boundaries and tests,
then adds route/account isolation, cancellation, bounded input handling, and
the OmniBridge error and registry contracts. The complete upstream license is
copied at the pinned provenance directory above.

Copyright (c) 2025 Jason Young.
