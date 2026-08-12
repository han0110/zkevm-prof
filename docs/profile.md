# zkEVM guest cost profiling

Measures what a zkVM charges to prove a stateless validator executing a block. The cost is the zkVM's
own price for the execution rather than wall clock time, so it reproduces on any machine and compares
across guests that ran the same blocks.

Nothing in the method depends on how a guest is written. A backend prices whatever program it is
handed and reads the heap that program left behind, so every guest on one zkVM is measured alike.

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

| Option | Values | Meaning |
| --- | --- | --- |
| `--zkvm` | `openvm`, `sp1`, `zisk` | zkVM the guest is profiled on. |
| `--stateless-validator` | the guests `elf-registry.json` lists | Guest to profile. |
| `--input` | path | Directory of EEST fixtures, walked recursively for `.json`. |
| `--output` | path | Where the profile JSON is written. |
| `--elf` | path | Guest ELF to profile, in place of the downloaded one. |

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

A backend reports the zkVM's own price for the execution and splits it into the kinds that zkVM
charges through. The split partitions that price rather than reading it a second way, so the kinds sum
to the total and every backend holds them to it while it runs.

ZisK prices an execution as a weighted sum over the work the prover has to do, which its emulator
accumulates as it runs. The emulator is driven in process rather than through the `ziskemu` binary,
and the resulting distribution matches `ziskemu -X` exactly. Its summary block carries the total
beside the rows that make it up, so the kinds are read out of the emulator rather than recomputed.

OpenVM prices an execution from trace geometry instead, through the same metered cost mode as
`cargo openvm run --mode meter`. Every instruction is charged the rows it adds times the width of the
AIR that proves it, so the total is the trace cells the app VM commits to. The number depends on the
app VM configuration, which is kept equal to the one ere proves these guests under. Execution goes
through rvr, which translates the program to C and builds it into a shared library once per artifact,
so metering more blocks stays cheap once that build is done.

That mode reports the total as one number, so a profile splits it by running the execution once per
kind against an artifact priced with only that kind's AIR widths. The cost is a weighted sum over the
AIRs an execution charges and the weights belong to the caller, so zeroing the rest leaves exactly the
cells one kind contributes. The accelerator extensions are recorded under `precompile` and whatever
runs in the base instruction set under `rv64`.

No instruction can add a row to the AIRs that argue memory consistency, to the lookup tables or to the
periphery hasher, so they belong to no kind. A third run prices every AIR and the kinds are held to
summing to it, which proves those AIRs stayed at zero rather than assuming it.

SP1 prices an execution as gas, which weighs the trace cells the AIRs an execution touches have to
commit to against the constraints those AIRs evaluate. A profile records their blended sum, since SP1
reports gas as that sum divided by 191 with a rounding step per chunk while the sum adds up to its own
parts exactly and ranks blocks identically.

The two weightings apply to every AIR alike, so they say what kind of prover work a block buys rather
than what the block did. A profile splits the total by the accumulators SP1 charges through instead,
recording precompiles under `syscall`, the chips the machine runs on the program's behalf under
`system` and instructions under `opcode`. Precompiles are charged in place or deferred to a later
shard depending on the retained event presets, which moves rows between two counters without repricing
them, so `syscall` covers both.

Gas moves with the size of the chunks the trace is cut into, since a chunk boundary is accounted a
shard boundary, so the profiler pins the cadence SP1 calibrated gas against. That cadence reserves
over 2 GiB of trace buffer per thread, well past what the other backends hold, so the pool needs
capping on a machine with little memory.

## Peak heap

A backend reads peak heap out of the guest memory its run left behind, knowing nothing about the
program beyond where its heap lies. Guest memory starts zeroed and an allocator hands memory back
without zeroing it, so the bytes left non-zero are the ones the guest reached, and the peak is the
span between the outermost of them. Taking a span rather than the distance from the heap's bottom
reads the same whether memory is handed out from the bottom up or from the top down, which is what
lets one reading serve an allocator growing in either direction.

Where the heap lies is a property of the zkVM's toolchain rather than of the program, so it is derived
per zkVM. ZisK guests carry `_kernel_heap_bottom` and `_kernel_heap_top`, since the stack sits between
the guest image and the heap and `_end` therefore marks the bottom of the stack. OpenVM puts the stack
below the image and runs the heap from `_end` to the end of its address space. An ELF missing the
symbol its zkVM delimits the heap with fails the run, since a guest quietly absent from the heap chart
reads as one that allocates nothing.

Most of what separates one zkVM's figure from another's for the same program is the allocator that
zkVM's toolchain links. An allocator that never reuses memory makes the machine hold everything ever
allocated, while one that recycles holds only the peak its arena reached, and the reading follows that
difference rather than hiding it.

One limit is worth knowing. Memory an allocator reserved but never touched is invisible, because
nothing distinguishes it from memory that was never handed out, so a program that bumps its cursor
over pages it never writes reads lower than its allocator would claim. Only the two ends of the
reading are exposed to this, since zeros between them are covered by the span, and the reading
otherwise sits within a page of the truth.

SP1 reaches its heap differently. Its executor serves guest memory a word at a time and its heap spans
110 GiB, too wide to read as a slice, so the backend drives the transpiler that executor wraps and
reads the memfd the resulting JIT runs the guest against. The pages that file holds are the pages the
guest reached and the kernel reports them by seeking between data and holes, which makes the reading
page granular where the others are byte exact. The last page of the span is left out, since the
allocator SP1 links caps its pool there before the guest starts. That JIT exists only on x86_64 Linux
and the reading goes with it, so a build for any other target records no heap on SP1.
