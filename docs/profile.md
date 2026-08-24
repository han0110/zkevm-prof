# zkEVM guest cost profiling

Measures what a zkVM charges to prove a stateless validator that executes a block. The cost is the
zkVM's own price for the execution, not wall clock time. It therefore reproduces on any machine, and
it compares guests that ran the same blocks.

A backend prices the program it receives and reads the memory that program left behind. Every guest
on one zkVM is therefore measured the same way, whatever it is written in.

```sh
cargo build --release
./target/release/zkevm-prof --help
```

## profile

```sh
zkevm-prof profile \
  --zkvm openvm \
  --stateless-validator zesu \
  --suite mainnet-25580000-1000
```

| Option                  | Values                                  | Meaning                                                   |
| ----------------------- | --------------------------------------- | --------------------------------------------------------- |
| `--zkvm`                | `openvm`, `sp1`, `zisk`                 | zkVM to profile the guest on.                             |
| `--stateless-validator` | the guests in `elf-registry.json`       | Guest to profile.                                         |
| `--suite`               | the corpora in `suite-registry.json`    | Corpus to profile. The profiler downloads it.             |
| `--input`               | path                                    | Directory of fixtures to profile in place of a download.  |
| `--filter`              | regular expression                      | Profile only the blocks whose name matches.               |
| `--output-dir`          | path                                    | Directory to write the profile to. `profiles` by default. |
| `--elf`                 | path                                    | Guest ELF to profile in place of the downloaded one.      |
| `--force`               | flag                                    | Profile a guest again although its run is published.      |

### Corpora

`suite-registry.json` gives the release each corpus comes from. Name a corpus with `--suite` and the
profiler downloads it.

The download streams from the release into `tar`, and `tar` extracts the corpus into
`fixtures/<suite>`. No second copy reaches the disk. The profiler keeps that directory, so it
downloads a corpus one time only. Set `GITHUB_TOKEN` to lift the rate limit on an anonymous
download.

A release caps one asset at 2 GiB, so the registry publishes a larger corpus in parts. The profiler
joins them in the order the registry lists. One archive can hold several corpora, and the registry
names the subdirectory to extract.

`--input` profiles a directory instead. The profiler downloads nothing. The corpus name is then the
directory name, unless `--suite` gives one.

### Part of a corpus

`--filter` profiles only the blocks whose name matches a regular expression. Use it for a span of
chain or a family of tests that has no corpus of its own.

A narrowed run is filed under the corpus it was cut from, and the page shows it as a run short of
that corpus. To profile the whole corpus afterwards, use `--force`.

Continuous integration narrows this way on a pull request. It profiles the first hundred blocks of
the mainnet corpus.

### Where a run is written

```
<output dir>/<stateless validator>/<ELF SHA-256, 16 digits>/<suite>.json
```

The path identifies a run by the guest, the exact binary and the corpus. The path does not name the
harness, which the file records instead. A new release therefore replaces the runs it measures again
rather than files a second copy beside them.

The profiler skips a run when the published file already records all of these.

- The harness version.
- The ELF and the URL it came from.
- The zkVM version.
- The guest version.

The profiler makes this check before it downloads the corpus and before it compiles the guest. A
skipped rerun therefore costs the ELF download and nothing more. `--force` measures the run again.

### Interrupted runs

A run writes what it measured to `.<suite>.json.checkpoint` beside the profile. It writes at most
once every five seconds, and only when it measured a new block. At the end it moves that checkpoint
onto the published path.

An interrupted run therefore keeps all but the last few seconds of its work. Run the same command
again on the same machine. It profiles only the blocks the checkpoint is short of. `--force` ignores
the checkpoint and measures the whole corpus again.

A recorded failure is carried forward. The profiler does not try that block again, because the guest
fails on it again.

Nothing reaches the published path until a whole run is ready, so a profile stays whole until the run
that replaces it finishes. A checkpoint is never published, because everything that reads a run reads
`.json` files alone.

### The profile file

The profiler profiles the last block of each fixture. That is the block an EEST blockchain test
exercises, and the blocks before it only build the state it runs against. A fixture that holds
several tests gives one entry per test.

Each entry carries the cost, keyed by the kinds that zkVM charges for. The kinds partition the
execution, so the whole it cost is what they sum to and no key holds it. On a zkVM whose backend
reads the heap, the entry also carries `peak_heap_bytes`. The profiler leaves that field out where it
read no heap.

A block the guest did not get through goes under `failures` with the reason the backend gave. A run
that failed on nothing writes no `failures` key at all.

The meta names the guest, the exact binary, the corpus and the kinds the cost splits into, so a
reader needs nothing else.

```json
{
  "profile": {
    "witness-generator-spec-cli::block_25580000_24c8fa4d": {
      "cost": { "precompile": 7154852252, "rv64": 10994655178 },
      "peak_heap_bytes": 57531938,
      "metadata": { "gas_used": 26211834, "block_number": 25580000 }
    }
  },
  "failures": {
    "witness-generator-spec-cli::block_25580001_7f0be412": {
      "reason": "the guest hit an unaligned operand",
      "metadata": { "gas_used": 29981210, "block_number": 25580001 }
    }
  },
  "meta": {
    "version": "0.1.0",
    "zkvm": "openvm",
    "zkvm_version": "v2.1.0-preview",
    "stateless_validator": "zesu",
    "stateless_validator_version": "glamsterdam-devnet-7",
    "elf_url": "https://github.com/...",
    "elf_sha256": "37c2fecb7645bb92...",
    "suite": "mainnet-25580000-1000",
    "generated_at": 1786772234,
    "run_url": null,
    "composition": [
      { "name": "precompile", "note": "Precompiles" },
      { "name": "rv64", "note": "RISC-V instructions" }
    ]
  }
}
```

### Parallel execution

The profiler runs blocks in parallel over the rayon global pool, which fills every core. One profiled
execution holds the whole guest memory. On a machine with many cores and little memory, cap the pool.

```sh
RAYON_NUM_THREADS=8 zkevm-prof profile --zkvm openvm ...
```

Progress and failures go to stderr. The profiler drops whatever a backend prints while it runs. It
reports each failed block, records it under `failures`, and prints how many blocks it got through.
The command fails only when every block failed.

## Cost

| zkVM   | Cost unit                        |
| ------ | -------------------------------- |
| OpenVM | `row * width`                    |
| SP1    | `row * (width * 3 + constraint)` |
| ZisK   | `row * width`                    |

### OpenVM

The cost unit is trace cells. It splits into two kinds.

- `precompile` is the accelerator extensions.
- `rv64` is the RISC-V instructions.

#### Source

- [Metered cost mode](https://github.com/openvm-org/openvm/blob/v2.1.0-preview/crates/vm/src/arch/execution_mode/metered_cost.rs)

### SP1

The cost unit is gas. Gas weighs trace cells against the constraints the AIRs evaluate. It splits
into three kinds.

- `syscall` is the accelerator chips that syscalls invoke.
- `system` is the memory chips and the global bus.
- `opcode` is the RISC-V instructions.

#### Source

- [Gas formula](https://github.com/succinctlabs/sp1/blob/v6.4.0/crates/core/executor/src/vm/gas.rs)
- [Trace cell table](https://github.com/succinctlabs/sp1/blob/v6.4.0/crates/core/executor/src/artifacts/rv64im_costs.json)
- [Constraint table](https://github.com/succinctlabs/sp1/blob/v6.4.0/crates/core/executor/src/artifacts/rv64im_complexity.json)

### ZisK

The cost unit is trace cells. It splits into five kinds.

- `base` is the ROM and the lookup tables at their fixed heights.
- `precompile` is the precompiles.
- `memory` is the memory operations.
- `opcode` is the ZisK instructions.
- `main` is the processor itself, one row per step.

#### Source

- [Emulator execution statistics](https://github.com/0xPolygonHermez/zisk/blob/v1.1.0-alpha/emulator/src/stats/stats.rs)
- [Base/Main/Memory weights](https://github.com/0xPolygonHermez/zisk/blob/v1.1.0-alpha/emulator/src/emu_costs.rs)
- [Operation weights](https://github.com/0xPolygonHermez/zisk/blob/v1.1.0-alpha/core/src/zisk_ops_costs.rs)

## Peak heap

Peak heap is the span between the outermost bytes a guest left non-zero in its heap. Guest memory
starts at zero, and an allocator does not zero what it frees. A non-zero byte is therefore a byte the
guest reached. The span reads the same whether the allocator grows up or down.

Each zkVM delimits the heap differently.

- OpenVM runs from `_end` to the end of the address space.
- SP1 runs from `_end` to the input region above it.
- ZisK runs from `_heap_bottom` to `_heap_top`.

The input buffer falls inside the reading on OpenVM only, which reads the input into the guest's
first allocation. SP1 and ZisK give the guest a pointer to a region outside the heap.

### What peak heap does not show

The allocator the toolchain links dominates any comparison across zkVMs. An allocator that never
reuses memory holds everything it ever allocated. One that recycles holds only the peak of its arena.

Memory that an allocator reserved but never touched is invisible. A guest that moves its cursor over
pages it never writes therefore reads lower than its allocator claims.

SP1 is read differently. Its heap spans 110 GiB of address space, which is too wide to slice. The
backend therefore reads the memfd that the JIT runs the guest against. A page the guest never reached
is a hole, and the file never materialises it. The reading is therefore page granular, where the
others are byte exact. The JIT exists on x86_64 Linux only, so a build for any other target records
no heap on SP1.
