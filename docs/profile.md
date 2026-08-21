# zkEVM guest cost profiling

Measures what a zkVM charges to prove a stateless validator executing a block. The cost is the zkVM's
own price for the execution rather than wall clock time, so it reproduces on any machine and compares
across guests that ran the same blocks.

Nothing in the method depends on how a guest is written. A backend prices whatever program it is
handed and reads the memory that program left behind, so every guest on one zkVM is measured alike.

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

| Option                  | Values                               | Meaning                                                          |
| ----------------------- | ------------------------------------ | ---------------------------------------------------------------- |
| `--zkvm`                | `openvm`, `sp1`, `zisk`              | zkVM the guest is profiled on.                                   |
| `--stateless-validator` | the guests `elf-registry.json` lists | Guest to profile.                                                |
| `--suite`               | the corpora `suite-registry.json` lists | Corpus to profile, fetched unless an input directory is given. |
| `--input`               | path                                 | Directory of EEST fixtures, walked recursively for `.json`, in place of the fetched corpus. |
| `--filter`              | regular expression                   | Profiles only the blocks whose name matches, in place of the whole corpus. |
| `--output-dir`          | path                                 | Directory the profile is published under, `profiles` by default. |
| `--elf`                 | path                                 | Guest ELF to profile, in place of the downloaded one.            |
| `--force`               | flag                                 | Reprofiles a guest whose run is already published.               |

`suite-registry.json` says where each corpus is published, so naming a suite is enough to profile it.
The archive streams from its release straight into tar, which extracts the corpus alone into
`fixtures/<suite>` and leaves no second copy on a disk the fixtures already fill. A corpus larger than
the 2 GiB a release caps one asset at is published in parts and concatenated in the order listed, and
one holding several corpora is extracted by the subdirectory the suite names. That directory is
reused on the next run, so a corpus is fetched once, and `GITHUB_TOKEN` lifts the rate limit an
anonymous download is held to where it is set. The fetch happens only after an already-published run
is skipped, so re-profiling an unchanged guest downloads nothing.

Passing `--input` profiles the directory given instead, fetching nothing, and the suite is then that
directory's name unless `--suite` states one.

`--filter` narrows either to the blocks whose key matches a regular expression, which is how a span of
chain or a family of tests is profiled without a corpus of its own. A run so narrowed is filed under
the corpus it was cut from and covers part of it, so it reads on the page as a run short of that
corpus, and profiling the whole afterwards takes `--force` since the path already carries a run of
this guest. Continuous integration narrows this way on a pull request, profiling the first hundred
blocks of the mainnet corpus rather than a corpus of its own.

The profile is written to
`<output dir>/<stateless validator>/<ELF SHA-256, 16 digits>/<suite>.json`, which identifies a run by
the guest, the exact binary and the corpus. The harness that measured it is recorded in the file
rather than named by the path, so a release supersedes the runs it re-measures instead of filing a
second copy of each beside them. A run whose file already records this harness version, that ELF,
that ELF URL, this zkVM version and this guest version is skipped, which is checked before the guest
is compiled so re-profiling an unchanged guest pays only for the download. `--force` measures it
again regardless.

Only the last block of each fixture is profiled, since that is the block an EEST blockchain test is
written to exercise and the ones before it only build the state it runs against. A fixture holding
several tests contributes one entry per test.

The output stands on its own. Each entry carries the cost keyed by the kinds that zkVM charges for
alongside the `total` they sum to, and `peak_heap_bytes` on a zkVM whose backend reads the heap,
omitting the field where none was read rather than recording a heap of nothing. A block the guest did
not get through is recorded under `failures` with what the backend reported, which is what a profile
short of its corpus is short by, and the key is left out entirely by a run that failed on nothing.
The meta names the guest, the exact binary a cost was measured against, the corpus it covers and the
kinds the cost map decomposes into, so nothing else has to be read to interpret it.

```json
{
  "profile": {
    "witness-generator-spec-cli::block_25580000_24c8fa4d": {
      "cost": { "precompile": 7154852252, "rv64": 10994655178, "total": 18149507430 },
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

Blocks are profiled in parallel over rayon's global pool, which by default fills every core. A
profiled execution holds the whole guest memory, so cap the pool on a machine with many cores and
little memory.

```sh
RAYON_NUM_THREADS=8 zkevm-prof profile --zkvm openvm ...
```

Progress and failures go to stderr, and whatever a backend prints on either stream while it runs is
dropped. A failed block is reported and recorded under `failures`, the run prints how many blocks it
got through, and the command fails only when every block failed.

## Cost

| zkVM   | Cost unit                        |
| ------ | -------------------------------- |
| OpenVM | `row * width`                    |
| SP1    | `row * (width * 3 + constraint)` |
| ZisK   | `row * width`                    |

### OpenVM

The cost unit is trace cells, and split into `precompile` and `rv64`.

- `precompile` - the accelerator extensions.
- `rv64` - the RISC-V instructions.

#### Source

- [Metered cost mode](https://github.com/openvm-org/openvm/blob/v2.1.0-preview/crates/vm/src/arch/execution_mode/metered_cost.rs)

### SP1

The cost unit is gas, which weighs trace cells against the constraints the AIRs evaluate, and split
into `syscall`, `system` and `opcode`.

- `syscall` - the accelerator chips invoked by syscalls.
- `system` - the memory chips and the global bus.
- `opcode` - the RISC-V instructions.

#### Source

- [Gas formula](https://github.com/succinctlabs/sp1/blob/v6.4.0/crates/core/executor/src/vm/gas.rs)
- [Trace cell table](https://github.com/succinctlabs/sp1/blob/v6.4.0/crates/core/executor/src/artifacts/rv64im_costs.json)
- [Constraint table](https://github.com/succinctlabs/sp1/blob/v6.4.0/crates/core/executor/src/artifacts/rv64im_complexity.json)

### ZisK

The cost unit is trace cells, and split into `base`, `precompile`, `memory`, `opcode` and `main`.

- `base` - the ROM and the lookup tables at their fixed heights.
- `precompile` - the precompiles.
- `memory` - the memory operations.
- `opcode` - the ZisK instructions.
- `main` - the processor itself, one row per step.

#### Source

- [Emulator execution statistics](https://github.com/0xPolygonHermez/zisk/blob/v1.1.0-alpha/emulator/src/stats/stats.rs)
- [Base/Main/Memory weights](https://github.com/0xPolygonHermez/zisk/blob/v1.1.0-alpha/emulator/src/emu_costs.rs)
- [Operation weights](https://github.com/0xPolygonHermez/zisk/blob/v1.1.0-alpha/core/src/zisk_ops_costs.rs)

## Peak heap

The span between the outermost bytes a guest left non-zero in its heap. Guest memory starts zeroed and
an allocator does not zero what it frees, so a non-zero byte is one the guest reached, and a span
reads the same whether the allocator grows up or down.

- OpenVM - `_end` to the end of the address space.
- SP1 - `_end` to the input region above it.
- ZisK - `_heap_bottom` to `_heap_top`.

The input buffer falls inside the reading only on OpenVM, which reads it into the guest's first
allocation. SP1 and ZisK hand the guest a pointer to a region outside the heap.

Two things move the figure. The allocator a toolchain links dominates any comparison across zkVMs,
since one that never reuses memory holds everything ever allocated where one that recycles holds only
its arena's peak. Memory an allocator reserved but never touched is invisible, so a guest that bumps
its cursor over pages it never writes reads lower than its allocator would claim.

SP1 is read differently. Its heap spans 110 GiB of address space, too wide to slice, so the backend
reads the memfd its JIT runs the guest against. A page the guest never reached is a hole the file
never materialises, which makes the reading page granular where the others are byte exact. That JIT
exists only on x86_64 Linux, so a build for any other target records no heap on SP1.
