# zkEVM guest cost profiling

Measures what a zkVM charges to prove a stateless validator executing a block, and turns a corpus of
those measurements into a comparison across guests. The cost is the zkVM's own price for the
execution rather than wall clock time, so it reproduces on any machine and compares across guests
that ran the same blocks.

`profile` measures one guest over a fixture corpus and writes JSON. `report` aggregates several of
those files into one document.

```sh
cargo build --release
./target/release/zkevm-prof --help
```

## profile

```sh
zkevm-prof profile \
  --zkvm zisk \
  --stateless-validator nethermind \
  --input fixtures/mainnet-25580000-1000 \
  --output nethermind-zisk.json
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

The output pairs the per-test costs with the zkVM that produced them, which is how the report knows
what a cost map's kinds mean. An entry also carries `peak_heap_bytes` on a zkVM whose backend reads
the heap, and omits the field where none was read rather than recording a heap of nothing.

```json
{
  "profile": {
    "witness-generator-spec-cli::block_25580000_24c8fa4d": {
      "cost": { "base": 293601280, "main": 25355756224, "total": 42895927337 },
      "peak_heap_bytes": 57531938,
      "metadata": { "gas_used": 26211834, "block_number": 25580000 }
    }
  },
  "meta": { "zkvm": "zisk" }
}
```

Blocks are profiled in parallel over rayon's global pool, which by default fills every core. A
profiled execution holds the whole guest memory, so cap the pool on a machine with many cores and
little memory.

```sh
RAYON_NUM_THREADS=8 zkevm-prof profile --zkvm zisk ...
```

Progress and failures go to stderr, and whatever a backend prints on either stream while it runs is
dropped. A failed block is reported and skipped, the run prints how many were skipped, and the
command fails only when every block failed.

## report

```sh
zkevm-prof report --input ethrex-zisk.json nethermind-zisk.json --output index.html
zkevm-prof report --input ethrex-zisk.json nethermind-zisk.json --output report.md --format md
```

| Option | Values | Meaning |
| --- | --- | --- |
| `--input` | one or more paths | Profile JSON files, one per guest. |
| `--output` | path | Where the report is written. |
| `--format` | `json`, `md` | Shape of the report, `json` by default. |

Each input contributes one series labelled by its file stem, so `nethermind-zisk.json` becomes the
series `nethermind-zisk`. Series are compared over the tests they all profiled, so a guest that
failed on some blocks does not appear cheaper for having skipped them. Every input has to come from
the same zkVM, since costs from different zkVMs are in different units.

`json` carries the series in the units they were profiled in, along with the zkVM version, the block
count, where each guest's ELF is published and the time and workflow run that produced them. Peak
heap is carried the same way in `heap_lines`, holding only the guests whose profiles recorded it, so
a report whose guests recorded none leaves that section off the page. A guest also carries the mean
of its peaks along with its ratio to the smallest, which is the one heap figure per guest both tables
show. That ratio is taken between the means rather than averaged per block, since an average of
ratios reads differently depending on which guest it is stated against. `md` renders the same figures
as tables, from [`templates/report.md`](../templates/report.md), for pasting into a pull request.

Cost is shown per block wherever it appears, dividing the total a guest carries by the block count so
it reads beside the mean peak heap rather than against a different span. That covers the composition
as well as the tables, since a kind's share of a total is its share of the mean.

The page itself is static and lives in [`site/`](../site). It loads one report per zkVM by name, so
`openvm.json` fills the OpenVM tab, `sp1.json` the SP1 tab and `zisk.json` the ZisK tab, and a tab
always shows one run's batch rather than a mix of two. Numbers are formatted in the page rather than
in the report, so the JSON stays the raw measurement. Opening a tab records it in the URL fragment,
which makes a tab linkable.

## Guests

[`elf-registry.json`](../elf-registry.json) lists, per zkVM, the stateless validators a guest ELF is
published for. An entry naming a version and a URL is downloaded from that URL and known by that
version, and an entry naming neither is downloaded from ere-guests and versioned by its catalog.
Adding a guest is adding an entry, and the profile workflow reads its matrix from the same file.

```json
{
    "zisk": [
        { "stateless-validator": "ethrex" },
        {
            "stateless-validator": "nethermind",
            "version": "glamsterdam-devnet-7",
            "url": "https://github.com/han0110/nethermind/releases/download/glamsterdam-devnet-7/stateless-validator-nethermind-zisk-v1.0.0-alpha.elf"
        }
    ]
}
```

Guests left to [ere-guests][ere-guests] are downloaded at the version `stateless-validator-catalog`
is pinned to in `Cargo.toml`, which the build script reads so the download and the linked catalog
cannot drift apart. A tag pin resolves to that release's assets, and a branch or revision pin to the
build artifacts of the commit it locks to. The stateless input schema is versioned with the guests,
so a corpus and a guest have to come from the same version or the guest rejects the input. Artifacts
are named after the zkVM SDK that built them, and that SDK has to be the one this crate links, so
moving to another version means bumping the ere-guests and `ere-catalog` pins together, since the
catalog is what names the SDK version.

The GitHub API serves build artifacts only to an authenticated caller, so a branch or revision pin
needs `GITHUB_TOKEN` to hold a token. ere-guests is public, so any token is enough. Artifacts also
expire on the repository's retention window, after which the pin has to move, while release assets
do not. Passing `--elf` bypasses the download entirely.

Not every guest is built for every zkVM. ere-guests compiles Ethrex and Reth for each zkVM it
supports and republishes Zesu from a Consensys release only for ZisK, so no Zesu guest exists on SP1
and the registry lists none. Nethermind is outside the catalog for every zkVM, and ere-guests has no
OpenVM Zesu build that runs, since the build it published predates the accelerators and traps on its
first setup instruction. Both come from forks under the `glamsterdam-devnet-7` tag. A URL names the
zkVM SDK its asset was built against, so republishing a fork against another SDK means updating the
entry.

Rebuilding the Nethermind guest runs the recipe the Nethermind release workflow uses, from a
Nethermind checkout.

```sh
cd src/Nethermind/Nethermind.Stateless.ZiskGuest
make build
```

The bflat container writes `libziskos.a` back into the artifacts directory as root, which makes a
second build fail with `MSB3021`. Restore ownership before rebuilding.

```sh
docker run --rm -v "$PWD/../artifacts/bin/Nethermind.Stateless.ZiskGuest/release":/w alpine:3 \
  chown -R "$(id -u):$(id -g)" /w
```

## Peak heap

A backend reads peak heap out of the guest memory its run left behind, knowing nothing about the
guest beyond where its heap lies. Guest memory starts zeroed and an allocator hands memory back
without zeroing it, so the bytes left non-zero are the ones the guest reached, and the peak is the
span between the outermost of them. Taking a span rather than the distance from the heap's bottom
reads the same whether memory is handed out from the bottom up or from the top down, which is what
lets one reading serve the Nethermind guest, whose allocator grows downward from the top.

Where the heap lies is a property of the zkVM's toolchain rather than of the guest, so it is derived
per zkVM rather than declared per guest. ZisK guests carry `_kernel_heap_bottom` and
`_kernel_heap_top`, since the stack sits between the guest image and the heap and `_end` therefore
marks the bottom of the stack. OpenVM puts the stack below the image and runs the heap from `_end`
to the end of its address space. A guest whose ELF is missing the symbol its zkVM delimits the heap
with fails the run, since a guest quietly absent from the heap chart reads as one that allocates
nothing.

Most of what separates one zkVM's figure from another's for the same guest is the allocator that
zkVM's toolchain links. An allocator that never reuses memory makes the machine hold everything the
guest ever allocated, while one that recycles holds only the peak its arena reached, and the reading
follows that difference rather than hiding it. Validating mainnet block 25580000, the Ethrex guest
reads 65.4 MB against the never-reusing allocator ZisK links and 23.9 MB against the recycling one
SP1 links, and the Reth guest reads 57.5 MB against 39.7 MB. The gap is the memory a guest buys by
never giving any back, so the ratio between the two is a measure of how much it allocates and
abandons rather than of the work it does.

One limit is worth knowing. Memory an allocator reserved but the guest never touched is invisible,
because nothing distinguishes it from memory that was never handed out, so a guest that bumps its
cursor over pages it never writes reads lower than its allocator would claim. Only the two ends of
the reading are exposed to this, since zeros between them are covered by the span, and the reading
otherwise sits within a page of the truth. Measured against the cursors the ZisK and OpenVM guests
happen to keep, the shortfall runs from 4 to 224 bytes, and SP1's page granular reading overshoots
by under 1.5 KiB.

SP1 reaches its heap differently. Its executor serves guest memory a word at a time and its heap
spans 110 GiB, too wide to read as a slice, so the backend drives the transpiler that executor wraps
and reads the memfd the resulting JIT runs the guest against. The pages that file holds are the
pages the guest reached and the kernel reports them by seeking between data and holes, which makes
the reading page granular where the others are byte exact. The last page of the span is left out,
since the allocator SP1 links caps its pool there before the guest starts. That JIT exists only on
x86_64 Linux and the reading goes with it, so a build for any other target records no heap on SP1.

## zkVMs

ZisK prices an execution as a weighted sum over the work the prover has to do, which its emulator
accumulates as it runs. The emulator is driven in process rather than through the `ziskemu` binary,
and the resulting distribution matches `ziskemu -X` exactly.

OpenVM prices an execution from trace geometry instead, through the same metered cost mode as
`cargo openvm run --mode meter`. Every instruction is charged the rows it adds times the width of the
AIR that proves it, so the total is the trace cells the app VM commits to. A profile records that
total under `cost`. The number depends on the app VM configuration, which is kept equal to the one
ere proves these guests under.

That mode reports the total as one number, so a profile splits it by running the execution once per
kind against an artifact priced with only that kind's AIR widths. The cost is a weighted sum over the
AIRs an execution charges and the weights belong to the caller, so zeroing the rest leaves exactly
the cells one kind contributes. The accelerator extensions are recorded under `precompile` and
whatever a guest runs in the base instruction set under `rv64`, which is the split that says how far
a guest leans on the zkVM rather than on its own code.

No instruction can add a row to the AIRs that argue memory consistency, to the lookup tables or to
the periphery hasher, so they belong to no kind. A third run prices every AIR and the kinds are held
to summing to it, which proves those AIRs stayed at zero rather than assuming it.

Running an OpenVM guest goes through rvr, which translates the program to C and builds it into a
shared library, once per artifact per ELF. That build needs LLVM clang 19 or newer and a matching
lld, installed by [`install-llvm.sh`](../.github/scripts/install-llvm.sh). It dominates a run and
does not grow with the corpus, so metering more blocks stays cheap once it is done.

SP1 prices an execution as gas, which weighs the trace cells the AIRs an execution touches have to
commit to against the constraints those AIRs evaluate. A profile records their blended sum under
`cost`. SP1 reports gas as that sum divided by 191 with a rounding step per chunk, so the profile
keeps the sum, which adds up to its own parts exactly and ranks blocks identically.

The two weightings apply to every AIR alike, so they say what kind of prover work a block buys rather
than what the block did. A profile splits the total by the accumulators SP1 charges through instead,
recording instructions under `opcode`, precompiles under `syscall` and the chips the machine runs on
the program's behalf under `system`. Precompiles are charged in place or deferred to a later shard
depending on the retained event presets, which moves rows between two counters without repricing
them, so `syscall` covers both.

Gas moves with the size of the chunks the trace is cut into, since a chunk boundary is accounted a
shard boundary, so the profiler pins the cadence SP1 calibrated gas against. That cadence reserves
over 2 GiB of trace buffer per thread, well past what the other backends hold, so the pool needs
capping on a machine with little memory.

Adding a zkVM means implementing `Profiler` in a module under `src/zkvm` and declaring how its cost
map decomposes. The cost is an open map of whatever kinds that zkVM charges for, so the fixture
walking, the output format and the report need no change.

```rust
pub const COMPOSITION: Composition = Composition {
    total: "cost",
    components: &[Kind {
        name: "memory",
        note: "reads and writes",
    }],
};
```

The report reads `meta.zkvm` and picks that backend's composition, so a profile never repeats the
shape of its own cost map. Each kind carries the note the report prints under the chart, which is
where a reader learns what the kind covers. A zkVM that prices an execution as one number leaves
`components` empty, and the report drops its composition section rather than drawing a bar of one
segment.

## Continuous integration

[`profile.yml`](../.github/workflows/profile.yml) is a reusable workflow that profiles a matrix of
guests over a mainnet corpus, aggregates the results and publishes the page to GitHub Pages from
`main`. It takes the zkVM as its only input and reads the guests from the registry, so adding a
backend means adding one caller. [`profile-zisk.yml`](../.github/workflows/profile-zisk.yml),
[`profile-openvm.yml`](../.github/workflows/profile-openvm.yml) and
[`profile-sp1.yml`](../.github/workflows/profile-sp1.yml) are those callers. Every job that
builds this crate first installs the LLVM toolchain rvr needs, since Ubuntu ships a clang older than
rvr accepts and no lld at all.

A pull request profiles 100 blocks and every other run profiles 1000. The corpus extracts to 15 GB,
more than a GitHub-hosted runner has free, so the job first runs
[`free-up-disk-space.sh`](../.github/scripts/free-up-disk-space.sh) to drop the preinstalled Android,
Haskell, .NET and CodeQL toolchains. Only the compressed archive is cached, keyed by its Drive file
id, so a rerun extracts it instead of downloading it again. The corpora and the cargo cache compete
for the 10 GB a repository gets, so only `main` writes and a branch restores what `main` last saved.

[`unit-test.yml`](../.github/workflows/unit-test.yml) runs `cargo fmt`, `clippy` and the tests on
every push and pull request, sharing the cargo cache with the profile workflow.

Every caller publishes the same static page to the root of the Pages branch alongside its own
`<zkvm>.json`, and keep_files leaves the other zkVMs' reports in place, so a run only ever replaces
its own tab. Callers share that branch, so the job holding the publish step takes one global
concurrency slot and their pushes cannot race.

[ere-guests]: https://github.com/eth-act/ere-guests
