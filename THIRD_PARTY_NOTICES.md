# Third-Party Notices

The native memory binary and TypeScript plugin include or link dependencies
under permissive licenses, including:

- Alibaba `zvec` and `zvec-rust` (Apache-2.0)
- UtilityAI `llama-cpp-rs` (MIT OR Apache-2.0)
- `llama.cpp` (MIT)
- Hugging Face `hf-hub` (Apache-2.0)
- Tokio `prost` (Apache-2.0)
- Buf Protobuf-ES (Apache-2.0 AND BSD-3-Clause)
- KeyHog `keyhog-core`, `keyhog-scanner`, and `keyhog-sources` (MIT OR Apache-2.0); scanner 0.5.44 is vendored with the RustSec-fixed `lru` dependency
- guardrail-rs `guardrail-core` and `guardrail-classifiers` (MIT OR Apache-2.0)
- Oxigraph (MIT OR Apache-2.0)
- Xberg (MIT), vendored at version 1.0.3 with dependency-bound compatibility patches
- Microsoft ONNX Runtime, loaded locally when Guardrail ONNX is configured (MIT)

The upstream zvec notice, including notices for bundled Unicode data and
pyglass-derived code, is redistributed at `notices/ZVEC_NOTICE`.
