# Retrieval Benchmark v1

This frozen smoke corpus compares the served output of four retrieval modes through the production Protobuf sidecar:

- `no-memory`
- `lexical`
- `dense`
- `hybrid`

The manifest pins the JSONL corpus hashes. The runner rejects modified fixtures, missing retrieval-mode capabilities, mode mismatches, and degraded searches with warnings.

Run it after staging a native build:

```sh
bun run build:native
OPENCODE_NATIVE_MEMORY_BIN="$PWD/target/debug/opencode-memory" \
OPENCODE_MEMORY_EMBEDDING_GPU_LAYERS=0 \
OPENCODE_MEMORY_EMBEDDING_THREADS=1 \
bun run benchmark:retrieval --modes no-memory,lexical,dense,hybrid
```

## Initial Baseline

Measured on 2026-07-26 with `Qwen3-Embedding-4B-Q4_K_M.gguf`, CPU-only, one embedding thread:

| Mode      | MRR@10 | nDCG@10 | Recall@10 | False abstention | No-answer specificity | p50 latency |
| --------- | -----: | ------: | --------: | ---------------: | --------------------: | ----------: |
| no-memory |  0.000 |   0.000 |     0.000 |            1.000 |                 1.000 |         n/a |
| lexical   |  0.700 |   0.712 |     0.700 |            0.200 |                 0.000 |     1.35 ms |
| dense     |  0.145 |   0.337 |     1.000 |            0.000 |                 0.000 | 1,548.64 ms |
| hybrid    |  0.750 |   0.788 |     0.900 |            0.000 |                 0.000 | 1,541.05 ms |

These numbers are an implementation smoke baseline, not a product-quality claim. The corpus has eight synthetic memories and six queries. In particular, zero no-answer specificity for all active modes exposes a calibration/abstention gap that a larger benchmark must investigate before score retuning.
