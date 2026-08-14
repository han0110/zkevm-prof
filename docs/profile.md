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
  --input fixtures/mainnet-25580000-1000 \
  --output zesu-openvm.json
```

| Option                  | Values                               | Meaning                                                     |
| ----------------------- | ------------------------------------ | ----------------------------------------------------------- |
| `--zkvm`                | `openvm`, `sp1`, `zisk`              | zkVM the guest is profiled on.                              |
| `--stateless-validator` | the guests `elf-registry.json` lists | Guest to profile.                                           |
| `--input`               | path                                 | Directory of EEST fixtures, walked recursively for `.json`. |
| `--output`              | path                                 | Where the profile JSON is written.                          |
| `--elf`                 | path                                 | Guest ELF to profile, in place of the downloaded one.       |

Only the last block of each fixture is profiled, since that is the block an EEST blockchain test is
written to exercise and the ones before it only build the state it runs against. A fixture holding
several tests contributes one entry per test.

The output pairs the per-test costs with the zkVM that produced them, which is how a reader knows what
a cost map's kinds mean. An entry also carries `peak_heap_bytes` on a zkVM whose backend reads the
heap, and omits the field where none was read rather than recording a heap of nothing.

```json
{
  "profile": {
    "witness-generator-spec-cli::block_25580000_24c8fa4d": {
      "cost": { "precompile": 7154852252, "rv64": 10994655178, "cost": 18149507430 },
      "peak_heap_bytes": 57531938,
      "metadata": { "gas_used": 26211834, "block_number": 25580000 }
    }
  },
  "meta": { "zkvm": "openvm" }
}
```

Blocks are profiled in parallel over rayon's global pool, which by default fills every core. A
profiled execution holds the whole guest memory, so cap the pool on a machine with many cores and
little memory.

```sh
RAYON_NUM_THREADS=8 zkevm-prof profile --zkvm openvm ...
```

Progress and failures go to stderr, and whatever a backend prints on either stream while it runs is
dropped. A failed block is reported and skipped, the run prints how many were skipped, and the command
fails only when every block failed.

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

- [Gas formula](https://github.com/succinctlabs/sp1/blob/v6.3.1/crates/core/executor/src/vm/gas.rs)
- [Trace cell table](https://github.com/succinctlabs/sp1/blob/v6.3.1/crates/core/executor/src/artifacts/rv64im_costs.json)
- [Constraint table](https://github.com/succinctlabs/sp1/blob/v6.3.1/crates/core/executor/src/artifacts/rv64im_complexity.json)

### ZisK

The cost unit is trace cells, and split into `base`, `precompile`, `memory`, `opcode` and `main`.

- `base` - the ROM and the lookup tables at their fixed heights.
- `precompile` - the precompiles.
- `memory` - the memory operations.
- `opcode` - the ZisK instructions.
- `main` - the processor itself, one row per step.

#### Source

- [Emulator execution statistics](https://github.com/0xPolygonHermez/zisk/blob/v1.0.0-alpha/emulator/src/stats/stats.rs)
- [Base/Main/Memory weights](https://github.com/0xPolygonHermez/zisk/blob/v1.0.0-alpha/emulator/src/emu_costs.rs)
- [Operation weights](https://github.com/0xPolygonHermez/zisk/blob/v1.0.0-alpha/core/src/zisk_ops_costs.rs)

## Peak heap

The span between the outermost bytes a guest left non-zero in its heap. Guest memory starts zeroed and
an allocator does not zero what it frees, so a non-zero byte is one the guest reached, and a span
reads the same whether the allocator grows up or down.

- OpenVM - `_end` to the end of the address space.
- SP1 - `_end` to the input region above it.
- ZisK - `_kernel_heap_bottom` to `_kernel_heap_top`.

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
